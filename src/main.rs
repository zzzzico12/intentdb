use anyhow::{Context, Result};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::{delete, get, post},
    Router,
};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

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
        text: String,
        #[arg(short, long = "tag")]
        tags: Vec<String>,
    },
    /// 自然言語で検索する
    Search {
        query: String,
        #[arg(short, long, default_value = "5")]
        top: usize,
        #[arg(short, long = "tag")]
        tags: Vec<String>,
    },
    /// 全件表示
    List {
        #[arg(short, long = "tag")]
        tags: Vec<String>,
    },
    /// データを削除する（IDの先頭8文字で指定）
    Delete { id: String },
    /// HTTP APIサーバーを起動する
    Serve {
        #[arg(short, long, default_value = "3000")]
        port: u16,
    },
}

// ─── データ構造 ─────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Debug, Clone)]
struct IntentRecord {
    id: String,
    text: String,
    vector: Vec<f32>,
    timestamp: u64,
    tags: Vec<String>,
}

// ─── .idbファイルフォーマット ───────────────────────────────────────────────
//
// [MAGIC: 4B "IDB2"][レコード数: u32]
// 各レコード:
//   [idの長さ: u16][id bytes]
//   [textの長さ: u32][text bytes]
//   [vector次元数: u32][f32 x N]
//   [timestamp: u64]
//   [tags数: u16][[tagの長さ: u16][tag bytes] x tags数]
//
// IDB1との差分: tagsフィールドが追加。IDB1読み込み時はtags=[]として扱う。

const MAGIC_V2: &[u8; 4] = b"IDB2";
const MAGIC_V1: &[u8; 4] = b"IDB1";

fn write_db(path: &PathBuf, records: &[IntentRecord]) -> Result<()> {
    let mut f = std::fs::File::create(path)?;
    f.write_all(MAGIC_V2)?;
    f.write_u32::<LittleEndian>(records.len() as u32)?;

    for rec in records {
        let id_bytes = rec.id.as_bytes();
        f.write_u16::<LittleEndian>(id_bytes.len() as u16)?;
        f.write_all(id_bytes)?;

        let text_bytes = rec.text.as_bytes();
        f.write_u32::<LittleEndian>(text_bytes.len() as u32)?;
        f.write_all(text_bytes)?;

        f.write_u32::<LittleEndian>(rec.vector.len() as u32)?;
        for &v in &rec.vector {
            f.write_f32::<LittleEndian>(v)?;
        }

        f.write_u64::<LittleEndian>(rec.timestamp)?;

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
        let id_len = f.read_u16::<LittleEndian>()? as usize;
        let mut id_bytes = vec![0u8; id_len];
        f.read_exact(&mut id_bytes)?;
        let id = String::from_utf8(id_bytes)?;

        let text_len = f.read_u32::<LittleEndian>()? as usize;
        let mut text_bytes = vec![0u8; text_len];
        f.read_exact(&mut text_bytes)?;
        let text = String::from_utf8(text_bytes)?;

        let dim = f.read_u32::<LittleEndian>()? as usize;
        let mut vector = Vec::with_capacity(dim);
        for _ in 0..dim {
            vector.push(f.read_f32::<LittleEndian>()?);
        }

        let timestamp = f.read_u64::<LittleEndian>()?;

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

async fn get_embedding(text: &str, api_key: &str) -> Result<Vec<f32>> {
    let client = reqwest::Client::new();
    let req = EmbedRequest {
        input: text.to_string(),
        model: "text-embedding-3-small".to_string(),
    };
    let resp: EmbedResponse = client
        .post("https://api.openai.com/v1/embeddings")
        .bearer_auth(api_key)
        .json(&req)
        .send()
        .await
        .context("OpenAI APIへの接続に失敗しました")?
        .json()
        .await
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

// ─── HTTP APIのState・型定義 ───────────────────────────────────────────────

struct AppState {
    db_path: PathBuf,
    api_key: String,
    lock: Mutex<()>,
}

#[derive(Deserialize)]
struct PutBody {
    text: String,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Serialize)]
struct PutResponse {
    id: String,
    text: String,
    tags: Vec<String>,
    total: usize,
}

#[derive(Deserialize)]
struct TagFilter {
    #[serde(default)]
    tag: Vec<String>,
}

#[derive(Serialize)]
struct RecordResponse {
    id: String,
    text: String,
    tags: Vec<String>,
    timestamp: u64,
}

impl From<&IntentRecord> for RecordResponse {
    fn from(r: &IntentRecord) -> Self {
        RecordResponse {
            id: r.id.clone(),
            text: r.text.clone(),
            tags: r.tags.clone(),
            timestamp: r.timestamp,
        }
    }
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
    #[serde(default = "default_top")]
    top: usize,
    #[serde(default)]
    tag: Vec<String>,
}

fn default_top() -> usize {
    5
}

#[derive(Serialize)]
struct SearchResult {
    score: f32,
    id: String,
    text: String,
    tags: Vec<String>,
    timestamp: u64,
}

type AppError = (StatusCode, String);

fn internal(e: anyhow::Error) -> AppError {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

// ─── HTTP ハンドラ ─────────────────────────────────────────────────────────

// POST /records
async fn handle_put(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PutBody>,
) -> Result<Json<PutResponse>, AppError> {
    let vector = get_embedding(&body.text, &state.api_key).await.map_err(internal)?;
    let id = uuid::Uuid::new_v4().to_string();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let _guard = state.lock.lock().await;
    let mut records = read_db(&state.db_path).map_err(internal)?;
    records.push(IntentRecord {
        id: id.clone(),
        text: body.text.clone(),
        vector,
        timestamp,
        tags: body.tags.clone(),
    });
    let total = records.len();
    write_db(&state.db_path, &records).map_err(internal)?;

    Ok(Json(PutResponse { id, text: body.text, tags: body.tags, total }))
}

// GET /records?tag=a&tag=b
async fn handle_list(
    State(state): State<Arc<AppState>>,
    Query(filter): Query<TagFilter>,
) -> Result<Json<Vec<RecordResponse>>, AppError> {
    let _guard = state.lock.lock().await;
    let records = read_db(&state.db_path).map_err(internal)?;
    let result = records
        .iter()
        .filter(|r| matches_tags(r, &filter.tag))
        .map(RecordResponse::from)
        .collect();
    Ok(Json(result))
}

// GET /search?q=...&top=5&tag=a
async fn handle_search(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchQuery>,
) -> Result<Json<Vec<SearchResult>>, AppError> {
    let query_vec = get_embedding(&params.q, &state.api_key).await.map_err(internal)?;

    let _guard = state.lock.lock().await;
    let records = read_db(&state.db_path).map_err(internal)?;

    let mut scored: Vec<(f32, &IntentRecord)> = records
        .iter()
        .filter(|r| matches_tags(r, &params.tag))
        .map(|r| (cosine_similarity(&query_vec, &r.vector), r))
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());

    let result = scored
        .iter()
        .take(params.top)
        .map(|(score, r)| SearchResult {
            score: *score,
            id: r.id.clone(),
            text: r.text.clone(),
            tags: r.tags.clone(),
            timestamp: r.timestamp,
        })
        .collect();
    Ok(Json(result))
}

// DELETE /records/:id
async fn handle_delete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let _guard = state.lock.lock().await;
    let records = read_db(&state.db_path).map_err(internal)?;
    let before = records.len();
    let remaining: Vec<IntentRecord> =
        records.into_iter().filter(|r| !r.id.starts_with(&id)).collect();

    if remaining.len() == before {
        return Err((StatusCode::NOT_FOUND, format!("ID「{}」が見つかりません", id)));
    }

    let deleted = before - remaining.len();
    write_db(&state.db_path, &remaining).map_err(internal)?;
    Ok(Json(serde_json::json!({ "deleted": deleted, "remaining": remaining.len() })))
}

// ─── メイン ────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let api_key = std::env::var("OPENAI_API_KEY")
        .context("環境変数 OPENAI_API_KEY が設定されていません\n例: export OPENAI_API_KEY=sk-...")?;

    match cli.command {
        Commands::Put { text, tags } => {
            println!("📥 embedding生成中...");
            let vector = get_embedding(&text, &api_key).await?;
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
            let query_vec = get_embedding(&query, &api_key).await?;
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
            let filtered: Vec<&IntentRecord> =
                records.iter().filter(|rec| matches_tags(rec, &tags)).collect();
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
            let remaining: Vec<IntentRecord> =
                records.into_iter().filter(|rec| !rec.id.starts_with(&id)).collect();
            if remaining.len() == before {
                anyhow::bail!("ID「{}」に一致するレコードが見つかりませんでした", id);
            }
            write_db(&cli.file, &remaining)?;
            println!("🗑️  削除しました（残り {} 件）", remaining.len());
        }

        Commands::Serve { port } => {
            let state = Arc::new(AppState {
                db_path: cli.file.clone(),
                api_key,
                lock: Mutex::new(()),
            });

            let app = Router::new()
                .route("/records", post(handle_put))
                .route("/records", get(handle_list))
                .route("/records/:id", delete(handle_delete))
                .route("/search", get(handle_search))
                .with_state(state);

            let addr = format!("0.0.0.0:{}", port);
            let listener = tokio::net::TcpListener::bind(&addr).await?;
            println!("🚀 IntentDB API サーバー起動");
            println!("   http://localhost:{}", port);
            println!("   DBファイル: {}", cli.file.display());
            println!();
            println!("エンドポイント:");
            println!("  POST   /records          レコード追加");
            println!("  GET    /records           全件取得 (?tag=xxx)");
            println!("  GET    /search            検索 (?q=xxx&top=5&tag=xxx)");
            println!("  DELETE /records/:id       削除");
            axum::serve(listener, app).await?;
        }
    }

    Ok(())
}
