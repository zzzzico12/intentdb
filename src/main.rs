use anyhow::{Context, Result};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::PathBuf;

// ─── CLI定義 ───────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "idb")]
#[command(about = "IntentDB - スキーマ不要・自然言語で使えるDB")]
struct Cli {
    /// DBファイルのパス
    #[arg(short, long, default_value = "data.idb")]
    file: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// データを追加する
    Put {
        /// 保存したいテキスト（自由形式）
        text: String,
        /// タグを付ける（複数指定可）
        #[arg(short, long = "tag")]
        tags: Vec<String>,
    },
    /// 自然言語で検索する
    Search {
        /// 検索クエリ
        query: String,
        /// 返す件数
        #[arg(short, long, default_value = "5")]
        top: usize,
        /// タグでフィルタリング（複数指定時はAND）
        #[arg(short, long = "tag")]
        tags: Vec<String>,
    },
    /// 全件表示
    List {
        /// タグでフィルタリング（複数指定時はAND）
        #[arg(short, long = "tag")]
        tags: Vec<String>,
    },
    /// データを削除する（IDの先頭8文字で指定）
    Delete {
        /// 削除するレコードのID（先頭8文字以上）
        id: String,
    },
}

// ─── データ構造 ─────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Debug, Clone)]
struct IntentRecord {
    id: String,
    text: String,
    vector: Vec<f32>, // 1536次元（OpenAI text-embedding-3-small）
    timestamp: u64,
    tags: Vec<String>,
}

// ─── .idbファイルフォーマット ───────────────────────────────────────────────
//
// [MAGIC: 4B "IDB2"]
// [レコード数: u32]
// 各レコード:
//   [idの長さ: u16][id bytes]
//   [textの長さ: u32][text bytes]
//   [vector次元数: u32][f32 x N]
//   [timestamp: u64]
//   [tags数: u16]
//     [tagの長さ: u16][tag bytes]  x tags数
//
// IDB1との差分: tagsフィールドが追加。IDB1読み込み時はtags=[]として扱う。

const MAGIC_V2: &[u8; 4] = b"IDB2";
const MAGIC_V1: &[u8; 4] = b"IDB1";

fn write_db(path: &PathBuf, records: &[IntentRecord]) -> Result<()> {
    let mut f = std::fs::File::create(path)?;

    f.write_all(MAGIC_V2)?;
    f.write_u32::<LittleEndian>(records.len() as u32)?;

    for rec in records {
        // id
        let id_bytes = rec.id.as_bytes();
        f.write_u16::<LittleEndian>(id_bytes.len() as u16)?;
        f.write_all(id_bytes)?;

        // text
        let text_bytes = rec.text.as_bytes();
        f.write_u32::<LittleEndian>(text_bytes.len() as u32)?;
        f.write_all(text_bytes)?;

        // vector
        f.write_u32::<LittleEndian>(rec.vector.len() as u32)?;
        for &v in &rec.vector {
            f.write_f32::<LittleEndian>(v)?;
        }

        // timestamp
        f.write_u64::<LittleEndian>(rec.timestamp)?;

        // tags
        f.write_u16::<LittleEndian>(rec.tags.len() as u16)?;
        for tag in &rec.tags {
            let tag_bytes = tag.as_bytes();
            f.write_u16::<LittleEndian>(tag_bytes.len() as u16)?;
            f.write_all(tag_bytes)?;
        }
    }

    Ok(())
}

fn read_db(path: &PathBuf) -> Result<Vec<IntentRecord>> {
    if !path.exists() {
        return Ok(vec![]);
    }

    let mut f = std::fs::File::open(path)?;

    let mut magic = [0u8; 4];
    f.read_exact(&mut magic)?;
    let has_tags = if &magic == MAGIC_V2 {
        true
    } else if &magic == MAGIC_V1 {
        false
    } else {
        anyhow::bail!("不正なファイル形式です（マジックバイト不一致）");
    };

    let count = f.read_u32::<LittleEndian>()? as usize;
    let mut records = Vec::with_capacity(count);

    for _ in 0..count {
        // id
        let id_len = f.read_u16::<LittleEndian>()? as usize;
        let mut id_bytes = vec![0u8; id_len];
        f.read_exact(&mut id_bytes)?;
        let id = String::from_utf8(id_bytes)?;

        // text
        let text_len = f.read_u32::<LittleEndian>()? as usize;
        let mut text_bytes = vec![0u8; text_len];
        f.read_exact(&mut text_bytes)?;
        let text = String::from_utf8(text_bytes)?;

        // vector
        let dim = f.read_u32::<LittleEndian>()? as usize;
        let mut vector = Vec::with_capacity(dim);
        for _ in 0..dim {
            vector.push(f.read_f32::<LittleEndian>()?);
        }

        // timestamp
        let timestamp = f.read_u64::<LittleEndian>()?;

        // tags（IDB1は空）
        let tags = if has_tags {
            let tag_count = f.read_u16::<LittleEndian>()? as usize;
            let mut tags = Vec::with_capacity(tag_count);
            for _ in 0..tag_count {
                let tag_len = f.read_u16::<LittleEndian>()? as usize;
                let mut tag_bytes = vec![0u8; tag_len];
                f.read_exact(&mut tag_bytes)?;
                tags.push(String::from_utf8(tag_bytes)?);
            }
            tags
        } else {
            vec![]
        };

        records.push(IntentRecord { id, text, vector, timestamp, tags });
    }

    Ok(records)
}

// ─── タグフィルタ ──────────────────────────────────────────────────────────

fn matches_tags(rec: &IntentRecord, filter: &[String]) -> bool {
    filter.iter().all(|t| rec.tags.contains(t))
}

fn format_tags(tags: &[String]) -> String {
    if tags.is_empty() {
        String::new()
    } else {
        format!(" [{}]", tags.join(", "))
    }
}

// ─── OpenAI Embedding API ──────────────────────────────────────────────────

#[derive(Serialize)]
struct EmbedRequest {
    input: String,
    model: String,
}

#[derive(Deserialize)]
struct EmbedResponse {
    data: Vec<EmbedData>,
}

#[derive(Deserialize)]
struct EmbedData {
    embedding: Vec<f32>,
}

fn get_embedding(text: &str, api_key: &str) -> Result<Vec<f32>> {
    let client = reqwest::blocking::Client::new();

    let req = EmbedRequest {
        input: text.to_string(),
        model: "text-embedding-3-small".to_string(),
    };

    let resp: EmbedResponse = client
        .post("https://api.openai.com/v1/embeddings")
        .bearer_auth(api_key)
        .json(&req)
        .send()
        .context("OpenAI APIへの接続に失敗しました")?
        .json()
        .context("OpenAI APIのレスポンス解析に失敗しました")?;

    Ok(resp.data.into_iter().next().unwrap().embedding)
}

// ─── コサイン類似度 ────────────────────────────────────────────────────────

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

// ─── メイン ────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();

    let api_key = std::env::var("OPENAI_API_KEY")
        .context("環境変数 OPENAI_API_KEY が設定されていません\n例: export OPENAI_API_KEY=sk-...")?;

    match cli.command {
        Commands::Put { text, tags } => {
            println!("📥 embedding生成中...");
            let vector = get_embedding(&text, &api_key)?;

            let mut records = read_db(&cli.file)?;
            let id = uuid::Uuid::new_v4().to_string();
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs();

            records.push(IntentRecord { id: id.clone(), text: text.clone(), vector, timestamp, tags: tags.clone() });
            write_db(&cli.file, &records)?;

            println!("✅ 保存しました（合計 {} 件）", records.len());
            println!("   ID: {}", &id[..8]);
            println!("   テキスト: {}", text);
            if !tags.is_empty() {
                println!("   タグ: {}", tags.join(", "));
            }
        }

        Commands::Search { query, top, tags } => {
            let records = read_db(&cli.file)?;
            if records.is_empty() {
                println!("DBにデータがありません。まず `idb put \"テキスト\"` で追加してください。");
                return Ok(());
            }

            println!("🔍 「{}」で検索中...", query);
            let query_vec = get_embedding(&query, &api_key)?;

            let mut scored: Vec<(f32, &IntentRecord)> = records
                .iter()
                .filter(|rec| matches_tags(rec, &tags))
                .map(|rec| (cosine_similarity(&query_vec, &rec.vector), rec))
                .collect();

            scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());

            if !tags.is_empty() {
                println!("   タグフィルタ: {}", tags.join(", "));
            }
            println!("\n結果（上位 {} 件）:", top.min(scored.len()));
            println!("{}", "─".repeat(50));

            for (i, (score, rec)) in scored.iter().take(top).enumerate() {
                println!("{}. [スコア: {:.3}]{}", i + 1, score, format_tags(&rec.tags));
                println!("   {}", rec.text);
                println!("   ID: {}...", &rec.id[..8]);
                println!();
            }
        }

        Commands::List { tags } => {
            let records = read_db(&cli.file)?;
            if records.is_empty() {
                println!("DBにデータがありません。");
                return Ok(());
            }

            let filtered: Vec<&IntentRecord> = records.iter().filter(|rec| matches_tags(rec, &tags)).collect();

            if !tags.is_empty() {
                println!("📋 {} 件（タグフィルタ: {}）", filtered.len(), tags.join(", "));
            } else {
                println!("📋 全 {} 件", filtered.len());
            }
            println!("{}", "─".repeat(50));

            for (i, rec) in filtered.iter().enumerate() {
                println!("{}. [{}...]{} {}", i + 1, &rec.id[..8], format_tags(&rec.tags), rec.text);
            }
        }

        Commands::Delete { id } => {
            let records = read_db(&cli.file)?;
            let before = records.len();
            let remaining: Vec<IntentRecord> = records
                .into_iter()
                .filter(|rec| !rec.id.starts_with(&id))
                .collect();

            if remaining.len() == before {
                anyhow::bail!("ID「{}」に一致するレコードが見つかりませんでした", id);
            }

            write_db(&cli.file, &remaining)?;
            println!("🗑️  削除しました（残り {} 件）", remaining.len());
        }
    }

    Ok(())
}
