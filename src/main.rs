mod hnsw;

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
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

// ─── CLI定義 ───────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "idb")]
#[command(about = "IntentDB - スキーマ不要・自然言語で使えるDB")]
struct Cli {
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
    /// 自然言語で検索する（HNSWインデックス使用）
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

const MAGIC_V2: &[u8; 4] = b"IDB2";
const MAGIC_V1: &[u8; 4] = b"IDB1";

fn write_db(path: &PathBuf, records: &[IntentRecord]) -> Result<()> {
    let mut f = std::fs::File::create(path)?;
    f.write_all(MAGIC_V2)?;
    f.write_u32::<LittleEndian>(records.len() as u32)?;
    for rec in records {
        let id_b = rec.id.as_bytes();
        f.write_u16::<LittleEndian>(id_b.len() as u16)?;
        f.write_all(id_b)?;
        let text_b = rec.text.as_bytes();
        f.write_u32::<LittleEndian>(text_b.len() as u32)?;
        f.write_all(text_b)?;
        f.write_u32::<LittleEndian>(rec.vector.len() as u32)?;
        for &v in &rec.vector {
            f.write_f32::<LittleEndian>(v)?;
        }
        f.write_u64::<LittleEndian>(rec.timestamp)?;
        f.write_u16::<LittleEndian>(rec.tags.len() as u16)?;
        for tag in &rec.tags {
            let tb = tag.as_bytes();
            f.write_u16::<LittleEndian>(tb.len() as u16)?;
            f.write_all(tb)?;
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
        let mut id_b = vec![0u8; id_len];
        f.read_exact(&mut id_b)?;
        let id = String::from_utf8(id_b)?;
        let text_len = f.read_u32::<LittleEndian>()? as usize;
        let mut text_b = vec![0u8; text_len];
        f.read_exact(&mut text_b)?;
        let text = String::from_utf8(text_b)?;
        let dim = f.read_u32::<LittleEndian>()? as usize;
        let mut vector = Vec::with_capacity(dim);
        for _ in 0..dim {
            vector.push(f.read_f32::<LittleEndian>()?);
        }
        let timestamp = f.read_u64::<LittleEndian>()?;
        let tags = if has_tags {
            let tc = f.read_u16::<LittleEndian>()? as usize;
            let mut tags = Vec::with_capacity(tc);
            for _ in 0..tc {
                let tl = f.read_u16::<LittleEndian>()? as usize;
                let mut tb = vec![0u8; tl];
                f.read_exact(&mut tb)?;
                tags.push(String::from_utf8(tb)?);
            }
            tags
        } else {
            vec![]
        };
        records.push(IntentRecord { id, text, vector, timestamp, tags });
    }
    Ok(records)
}

// ─── HNSWヘルパー ─────────────────────────────────────────────────────────

fn hnsw_path(db_path: &PathBuf) -> PathBuf {
    db_path.with_extension("hnsw")
}

/// HNSWインデックスを読み込む。存在しないまたは件数不一致なら再構築して保存。
fn load_or_build_hnsw(db_path: &PathBuf, records: &[IntentRecord]) -> Result<hnsw::Hnsw> {
    let hp = hnsw_path(db_path);
    let index = hnsw::Hnsw::load(&hp)?;
    if index.len() == records.len() {
        return Ok(index);
    }
    if !records.is_empty() {
        eprintln!("🔧 インデックスを構築中 ({} 件)...", records.len());
    }
    let index = hnsw::Hnsw::build(records.iter().map(|r| (r.id.clone(), r.vector.clone())));
    index.save(&hp)?;
    Ok(index)
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
    let req = EmbedRequest { input: text.to_string(), model: "text-embedding-3-small".to_string() };
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

// ─── HTTP APIのState・型定義 ───────────────────────────────────────────────

struct DbState {
    records: Vec<IntentRecord>,
    index: hnsw::Hnsw,
}

struct AppState {
    db_path: PathBuf,
    hnsw_path: PathBuf,
    api_key: String,
    db: Mutex<DbState>,
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
        RecordResponse { id: r.id.clone(), text: r.text.clone(), tags: r.tags.clone(), timestamp: r.timestamp }
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
    // embeddingはロックの外で取得（ネットワーク待ちをロック中に行わない）
    let vector = get_embedding(&body.text, &state.api_key).await.map_err(internal)?;
    let id = uuid::Uuid::new_v4().to_string();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let mut db = state.db.lock().await;
    db.index.insert(id.clone(), vector.clone());
    db.records.push(IntentRecord { id: id.clone(), text: body.text.clone(), vector, timestamp, tags: body.tags.clone() });
    let total = db.records.len();
    write_db(&state.db_path, &db.records).map_err(internal)?;
    db.index.save(&state.hnsw_path).map_err(internal)?;

    Ok(Json(PutResponse { id, text: body.text, tags: body.tags, total }))
}

// GET /records?tag=a&tag=b
async fn handle_list(
    State(state): State<Arc<AppState>>,
    Query(filter): Query<TagFilter>,
) -> Result<Json<Vec<RecordResponse>>, AppError> {
    let db = state.db.lock().await;
    let result = db.records.iter().filter(|r| matches_tags(r, &filter.tag)).map(RecordResponse::from).collect();
    Ok(Json(result))
}

// GET /search?q=...&top=5&tag=a
async fn handle_search(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchQuery>,
) -> Result<Json<Vec<SearchResult>>, AppError> {
    let query_vec = get_embedding(&params.q, &state.api_key).await.map_err(internal)?;

    let db = state.db.lock().await;
    let record_map: HashMap<&str, &IntentRecord> =
        db.records.iter().map(|r| (r.id.as_str(), r)).collect();

    // HNSWで検索後、タグフィルタを適用
    let raw = db.index.search(&query_vec, params.top * 4, 50); // タグフィルタ後に top 件残るよう多めに取る
    let result: Vec<SearchResult> = raw
        .iter()
        .filter_map(|&(score, id)| record_map.get(id).map(|rec| (score, *rec)))
        .filter(|(_, rec)| matches_tags(rec, &params.tag))
        .take(params.top)
        .map(|(score, rec)| SearchResult {
            score,
            id: rec.id.clone(),
            text: rec.text.clone(),
            tags: rec.tags.clone(),
            timestamp: rec.timestamp,
        })
        .collect();
    Ok(Json(result))
}

// DELETE /records/:id
async fn handle_delete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let mut db = state.db.lock().await;
    let before = db.records.len();
    db.records.retain(|r| !r.id.starts_with(&id));
    let deleted = before - db.records.len();

    if deleted == 0 {
        return Err((StatusCode::NOT_FOUND, format!("ID「{}」が見つかりません", id)));
    }

    // HNSWインデックスを再構築
    db.index = hnsw::Hnsw::build(db.records.iter().map(|r| (r.id.clone(), r.vector.clone())));
    write_db(&state.db_path, &db.records).map_err(internal)?;
    db.index.save(&state.hnsw_path).map_err(internal)?;

    let remaining = db.records.len();
    Ok(Json(serde_json::json!({ "deleted": deleted, "remaining": remaining })))
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
            let id = uuid::Uuid::new_v4().to_string();
            let timestamp =
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();

            let mut records = read_db(&cli.file)?;
            records.push(IntentRecord {
                id: id.clone(),
                text: text.clone(),
                vector: vector.clone(),
                timestamp,
                tags: tags.clone(),
            });
            write_db(&cli.file, &records)?;

            // HNSWインデックスに追記
            let hp = hnsw_path(&cli.file);
            let mut index = hnsw::Hnsw::load(&hp)?;
            if index.len() != records.len() - 1 {
                // 不一致なら全件で再構築
                index = hnsw::Hnsw::build(
                    records[..records.len() - 1].iter().map(|r| (r.id.clone(), r.vector.clone())),
                );
            }
            index.insert(id.clone(), vector);
            index.save(&hp)?;

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
            let index = load_or_build_hnsw(&cli.file, &records)?;

            let record_map: HashMap<&str, &IntentRecord> =
                records.iter().map(|r| (r.id.as_str(), r)).collect();

            // タグフィルタ後に top 件残るよう多めに検索
            let raw = index.search(&query_vec, top * 4, 50);
            let scored: Vec<(f32, &IntentRecord)> = raw
                .iter()
                .filter_map(|&(score, id)| record_map.get(id).map(|rec| (score, *rec)))
                .filter(|(_, rec)| matches_tags(rec, &tags))
                .take(top)
                .collect();

            if !tags.is_empty() {
                println!("   タグフィルタ: {}", tags.join(", "));
            }
            println!("\n結果（上位 {} 件）:", scored.len());
            println!("{}", "─".repeat(50));
            for (i, (score, rec)) in scored.iter().enumerate() {
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
            let mut records = read_db(&cli.file)?;
            let before = records.len();
            records.retain(|r| !r.id.starts_with(&id));
            if records.len() == before {
                anyhow::bail!("ID「{}」に一致するレコードが見つかりませんでした", id);
            }
            write_db(&cli.file, &records)?;

            // 削除後はHNSWを再構築
            let hp = hnsw_path(&cli.file);
            let index = hnsw::Hnsw::build(records.iter().map(|r| (r.id.clone(), r.vector.clone())));
            index.save(&hp)?;
            println!("🗑️  削除しました（残り {} 件）", records.len());
        }

        Commands::Serve { port } => {
            let records = read_db(&cli.file)?;
            let hp = hnsw_path(&cli.file);
            let index = load_or_build_hnsw(&cli.file, &records)?;

            let state = Arc::new(AppState {
                db_path: cli.file.clone(),
                hnsw_path: hp,
                api_key,
                db: Mutex::new(DbState { records, index }),
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
