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
    },
    /// 自然言語で検索する
    Search {
        /// 検索クエリ
        query: String,
        /// 返す件数
        #[arg(short, long, default_value = "5")]
        top: usize,
    },
    /// 全件表示
    List,
    /// データを削除する（IDの先頭8文字で指定）
    Delete {
        /// 削除するレコードのID（先頭8文字以上）
        id: String,
    },
}

// ─── データ構造 ─────────────────────────────────────────────────────────────

/// 1レコード = テキスト + ベクトル（embedding）
/// .idbファイルに独自バイナリ形式で保存する
#[derive(Serialize, Deserialize, Debug, Clone)]
struct IntentRecord {
    id: String,
    text: String,
    vector: Vec<f32>, // 1536次元（OpenAI text-embedding-3-small）
    timestamp: u64,
}

// ─── .idbファイルフォーマット ───────────────────────────────────────────────
//
// [MAGIC: 4B "IDB1"]
// [レコード数: u32]
// [レコード1][レコード2]...
//
// 各レコード:
// [idの長さ: u16][id bytes]
// [textの長さ: u32][text bytes]
// [vector次元数: u32][f32 x N]
// [timestamp: u64]

const MAGIC: &[u8; 4] = b"IDB1";

fn write_db(path: &PathBuf, records: &[IntentRecord]) -> Result<()> {
    let mut f = std::fs::File::create(path)?;

    f.write_all(MAGIC)?;
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
    }

    Ok(())
}

fn read_db(path: &PathBuf) -> Result<Vec<IntentRecord>> {
    if !path.exists() {
        return Ok(vec![]);
    }

    let mut f = std::fs::File::open(path)?;

    // マジックバイト確認
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic)?;
    if &magic != MAGIC {
        anyhow::bail!("不正なファイル形式です（マジックバイト不一致）");
    }

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

        records.push(IntentRecord { id, text, vector, timestamp });
    }

    Ok(records)
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

// ─── コサイン類似度（検索の核心） ─────────────────────────────────────────

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

    // APIキーを環境変数から取得
    let api_key = std::env::var("OPENAI_API_KEY")
        .context("環境変数 OPENAI_API_KEY が設定されていません\n例: export OPENAI_API_KEY=sk-...")?;

    match cli.command {
        Commands::Put { text } => {
            println!("📥 embedding生成中...");
            let vector = get_embedding(&text, &api_key)?;

            let mut records = read_db(&cli.file)?;
            let id = uuid::Uuid::new_v4().to_string();
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs();

            records.push(IntentRecord { id: id.clone(), text: text.clone(), vector, timestamp });
            write_db(&cli.file, &records)?;

            println!("✅ 保存しました（合計 {} 件）", records.len());
            println!("   ID: {}", &id[..8]);
            println!("   テキスト: {}", text);
        }

        Commands::Search { query, top } => {
            let records = read_db(&cli.file)?;
            if records.is_empty() {
                println!("DBにデータがありません。まず `idb put \"テキスト\"` で追加してください。");
                return Ok(());
            }

            println!("🔍 「{}」で検索中...", query);
            let query_vec = get_embedding(&query, &api_key)?;

            // コサイン類似度でスコアリング
            let mut scored: Vec<(f32, &IntentRecord)> = records
                .iter()
                .map(|rec| (cosine_similarity(&query_vec, &rec.vector), rec))
                .collect();

            scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());

            println!("\n結果（上位 {} 件）:", top.min(scored.len()));
            println!("{}", "─".repeat(50));

            for (i, (score, rec)) in scored.iter().take(top).enumerate() {
                println!("{}. [スコア: {:.3}]", i + 1, score);
                println!("   {}", rec.text);
                println!("   ID: {}...", &rec.id[..8]);
                println!();
            }
        }

        Commands::List => {
            let records = read_db(&cli.file)?;
            if records.is_empty() {
                println!("DBにデータがありません。");
                return Ok(());
            }

            println!("📋 全 {} 件", records.len());
            println!("{}", "─".repeat(50));

            for (i, rec) in records.iter().enumerate() {
                println!("{}. [{}...] {}", i + 1, &rec.id[..8], rec.text);
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
