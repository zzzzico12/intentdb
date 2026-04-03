mod hnsw;

use anyhow::{Context, Result};
use axum::{
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    response::Json,
    routing::{delete, get, post},
    Router,
};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
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
    /// 既存レコードを更新する（IDの先頭8文字で指定）
    Update {
        /// 更新するレコードのID（先頭8文字以上）
        id: String,
        /// 新しいテキスト
        text: String,
        /// タグを上書きする（省略時は既存タグを維持）
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
    /// IDに近いレコードを探す
    Related {
        /// 起点にするレコードのID（先頭8文字以上）
        id: String,
        #[arg(short, long, default_value = "5")]
        top: usize,
    },
    /// 全件表示
    List {
        #[arg(short, long = "tag")]
        tags: Vec<String>,
    },
    /// データを削除する（IDの先頭8文字で指定）
    Delete { id: String },
    /// ファイルから一括インポートする（JSON / CSV / TXT）
    Import {
        /// インポートするファイルのパス
        path: PathBuf,
        /// フォーマット（json | csv | txt）。省略時は拡張子から自動判定
        #[arg(short, long)]
        format: Option<String>,
    },
    /// DBをファイルにエクスポートする
    Export {
        /// 出力先ファイルのパス（省略時は標準出力）
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// フォーマット（json | csv）。デフォルト: json
        #[arg(short, long, default_value = "json")]
        format: String,
    },
    /// 重複レコードを検出する
    Dedup {
        /// 類似度がこの値以上なら重複とみなす（0.0〜1.0）
        #[arg(long, default_value = "0.95")]
        threshold: f32,
        /// 重複を自動削除する（新しいほうを残す）
        #[arg(long)]
        delete: bool,
    },
    /// HTTP APIサーバーを起動する
    Serve {
        #[arg(short, long, default_value = "3000")]
        port: u16,
        /// バインドするホスト（デフォルト: 127.0.0.1）
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
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

// インポートファイルの1行
#[derive(Deserialize)]
struct ImportEntry {
    text: String,
    #[serde(default)]
    tags: Vec<String>,
}

// エクスポート用（ベクトルは除く）
#[derive(Serialize)]
struct ExportEntry<'a> {
    id: &'a str,
    text: &'a str,
    tags: &'a Vec<String>,
    timestamp: u64,
}

// ─── .idbファイルフォーマット ───────────────────────────────────────────────

const MAGIC_V2: &[u8; 4] = b"IDB2";
const MAGIC_V1: &[u8; 4] = b"IDB1";

// read_db での単一フィールドの最大サイズ（破損・細工ファイルによるOOM防止）
const MAX_TEXT_BYTES: usize = 10 * 1024 * 1024; // 10MB
const MAX_VECTOR_DIM: usize = 16_384;            // 最大16384次元
const MAX_TAG_COUNT: usize = 1_024;
const MAX_TAG_BYTES: usize = 4_096;
const MAX_RECORDS: usize = 10_000_000;           // 1000万件
// HTTP API 入力制限
const MAX_INPUT_TEXT: usize = 32_768;            // 32KB（OpenAI制限は約300KB相当だが実用的な上限）
const MAX_TAG_INPUT: usize = 256;
const MAX_TAGS_COUNT: usize = 64;

fn write_db(path: &Path, records: &[IntentRecord]) -> Result<()> {
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

fn read_db(path: &Path) -> Result<Vec<IntentRecord>> {
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
        anyhow::bail!("invalid file format (magic bytes mismatch)");
    };
    let count = f.read_u32::<LittleEndian>()? as usize;
    anyhow::ensure!(count <= MAX_RECORDS, "record count too large: {}", count);
    let mut records = Vec::with_capacity(count.min(4096));
    for _ in 0..count {
        let id_len = f.read_u16::<LittleEndian>()? as usize;
        let mut id_b = vec![0u8; id_len];
        f.read_exact(&mut id_b)?;
        let id = String::from_utf8(id_b)?;

        let text_len = f.read_u32::<LittleEndian>()? as usize;
        anyhow::ensure!(text_len <= MAX_TEXT_BYTES, "text field too large: {} bytes", text_len);
        let mut text_b = vec![0u8; text_len];
        f.read_exact(&mut text_b)?;
        let text = String::from_utf8(text_b)?;

        let dim = f.read_u32::<LittleEndian>()? as usize;
        anyhow::ensure!(dim <= MAX_VECTOR_DIM, "vector dimension too large: {}", dim);
        let mut vector = Vec::with_capacity(dim);
        for _ in 0..dim {
            vector.push(f.read_f32::<LittleEndian>()?);
        }

        let timestamp = f.read_u64::<LittleEndian>()?;
        let tags = if has_tags {
            let tc = f.read_u16::<LittleEndian>()? as usize;
            anyhow::ensure!(tc <= MAX_TAG_COUNT, "tag count too large: {}", tc);
            let mut tags = Vec::with_capacity(tc);
            for _ in 0..tc {
                let tl = f.read_u16::<LittleEndian>()? as usize;
                anyhow::ensure!(tl <= MAX_TAG_BYTES, "tag too large: {} bytes", tl);
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

fn hnsw_path(db_path: &Path) -> PathBuf {
    db_path.with_extension("hnsw")
}

fn load_or_build_hnsw(db_path: &Path, records: &[IntentRecord]) -> Result<hnsw::Hnsw> {
    let hp = hnsw_path(db_path);
    let index = hnsw::Hnsw::load(&hp)?;
    if index.len() == records.len() {
        return Ok(index);
    }
    if !records.is_empty() {
        eprintln!("🔧 building index ({} records)...", records.len());
    }
    let index =
        hnsw::Hnsw::build(records.iter().map(|r| (r.id.clone(), r.vector.clone())));
    index.save(&hp)?;
    Ok(index)
}

fn rebuild_and_save_hnsw(db_path: &Path, records: &[IntentRecord]) -> Result<hnsw::Hnsw> {
    let index =
        hnsw::Hnsw::build(records.iter().map(|r| (r.id.clone(), r.vector.clone())));
    index.save(&hnsw_path(db_path))?;
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

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
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
        .context("failed to connect to OpenAI API")?
        .json()
        .await
        .context("failed to parse OpenAI API response")?;
    resp.data.into_iter().next()
        .ok_or_else(|| anyhow::anyhow!("OpenAI returned empty embedding data"))
        .map(|d| d.embedding)
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

#[derive(Deserialize)]
struct RelatedQuery {
    #[serde(default = "default_top")]
    top: usize,
}

#[derive(Deserialize)]
struct DedupQuery {
    #[serde(default = "default_threshold")]
    threshold: f32,
}

fn default_top() -> usize { 5 }
fn default_threshold() -> f32 { 0.95 }

#[derive(Serialize)]
struct DedupPair {
    score: f32,
    a: RecordResponse,
    b: RecordResponse,
}

#[derive(Deserialize)]
struct UpdateBody {
    text: String,
    #[serde(default)]
    tags: Vec<String>,
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
    // 内部エラーの詳細は外部に露出しない
    eprintln!("internal error: {:#}", e);
    (StatusCode::INTERNAL_SERVER_ERROR, "internal server error".to_string())
}

// ─── HTTP ハンドラ ─────────────────────────────────────────────────────────

fn validate_text(text: &str) -> Result<(), AppError> {
    if text.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "text must not be empty".to_string()));
    }
    if text.len() > MAX_INPUT_TEXT {
        return Err((StatusCode::BAD_REQUEST, format!("text too long (max {} bytes)", MAX_INPUT_TEXT)));
    }
    Ok(())
}

fn validate_tags(tags: &[String]) -> Result<(), AppError> {
    if tags.len() > MAX_TAGS_COUNT {
        return Err((StatusCode::BAD_REQUEST, format!("too many tags (max {})", MAX_TAGS_COUNT)));
    }
    for tag in tags {
        if tag.len() > MAX_TAG_INPUT {
            return Err((StatusCode::BAD_REQUEST, format!("tag too long (max {} bytes)", MAX_TAG_INPUT)));
        }
    }
    Ok(())
}

async fn handle_put(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PutBody>,
) -> Result<Json<PutResponse>, AppError> {
    validate_text(&body.text)?;
    validate_tags(&body.tags)?;
    let vector = get_embedding(&body.text, &state.api_key).await.map_err(internal)?;
    let id = uuid::Uuid::new_v4().to_string();
    let timestamp = now_secs();

    let mut db = state.db.lock().await;
    db.index.insert(id.clone(), vector.clone());
    db.records.push(IntentRecord {
        id: id.clone(),
        text: body.text.clone(),
        vector,
        timestamp,
        tags: body.tags.clone(),
    });
    let total = db.records.len();
    write_db(&state.db_path, &db.records).map_err(internal)?;
    db.index.save(&state.hnsw_path).map_err(internal)?;
    Ok(Json(PutResponse { id, text: body.text, tags: body.tags, total }))
}

async fn handle_list(
    State(state): State<Arc<AppState>>,
    Query(filter): Query<TagFilter>,
) -> Result<Json<Vec<RecordResponse>>, AppError> {
    let db = state.db.lock().await;
    let result = db
        .records
        .iter()
        .filter(|r| matches_tags(r, &filter.tag))
        .map(RecordResponse::from)
        .collect();
    Ok(Json(result))
}

async fn handle_search(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchQuery>,
) -> Result<Json<Vec<SearchResult>>, AppError> {
    validate_text(&params.q)?;
    if params.top == 0 || params.top > 100 {
        return Err((StatusCode::BAD_REQUEST, "top must be between 1 and 100".to_string()));
    }
    let query_vec = get_embedding(&params.q, &state.api_key).await.map_err(internal)?;
    let db = state.db.lock().await;
    let record_map: HashMap<&str, &IntentRecord> =
        db.records.iter().map(|r| (r.id.as_str(), r)).collect();
    let raw = db.index.search(&query_vec, params.top * 4, 50);
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

async fn handle_delete(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let mut db = state.db.lock().await;
    let before = db.records.len();
    db.records.retain(|r| !r.id.starts_with(&id));
    let deleted = before - db.records.len();
    if deleted == 0 {
        return Err((StatusCode::NOT_FOUND, format!("record not found: {}", id)));
    }
    db.index =
        hnsw::Hnsw::build(db.records.iter().map(|r| (r.id.clone(), r.vector.clone())));
    write_db(&state.db_path, &db.records).map_err(internal)?;
    db.index.save(&state.hnsw_path).map_err(internal)?;
    let remaining = db.records.len();
    Ok(Json(serde_json::json!({ "deleted": deleted, "remaining": remaining })))
}

// PATCH /records/:id
async fn handle_update(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<UpdateBody>,
) -> Result<Json<RecordResponse>, AppError> {
    validate_text(&body.text)?;
    validate_tags(&body.tags)?;
    let vector = get_embedding(&body.text, &state.api_key).await.map_err(internal)?;

    let mut db = state.db.lock().await;
    let rec = db.records.iter_mut().find(|r| r.id.starts_with(&id))
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("record not found: {}", id)))?;

    rec.text = body.text.clone();
    rec.vector = vector;
    if !body.tags.is_empty() {
        rec.tags = body.tags.clone();
    }
    rec.timestamp = now_secs();
    let response = RecordResponse::from(&*rec);

    db.index = hnsw::Hnsw::build(db.records.iter().map(|r| (r.id.clone(), r.vector.clone())));
    write_db(&state.db_path, &db.records).map_err(internal)?;
    db.index.save(&state.hnsw_path).map_err(internal)?;
    Ok(Json(response))
}

// GET /records/:id/related?top=5
async fn handle_related(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Query(params): Query<RelatedQuery>,
) -> Result<Json<Vec<SearchResult>>, AppError> {
    if params.top == 0 || params.top > 100 {
        return Err((StatusCode::BAD_REQUEST, "top must be between 1 and 100".to_string()));
    }
    let db = state.db.lock().await;
    let target = db.records.iter().find(|r| r.id.starts_with(&id))
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("record not found: {}", id)))?;

    let target_vec = target.vector.clone();
    let target_id = target.id.clone();
    let record_map: HashMap<&str, &IntentRecord> =
        db.records.iter().map(|r| (r.id.as_str(), r)).collect();

    let raw = db.index.search(&target_vec, params.top + 1, 50);
    let result: Vec<SearchResult> = raw
        .iter()
        .filter_map(|&(score, rid)| record_map.get(rid).map(|rec| (score, *rec)))
        .filter(|(_, rec)| rec.id != target_id)
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

// GET /dedup?threshold=0.95
async fn handle_dedup(
    State(state): State<Arc<AppState>>,
    Query(params): Query<DedupQuery>,
) -> Result<Json<Vec<DedupPair>>, AppError> {
    if !(0.0..=1.0).contains(&params.threshold) {
        return Err((StatusCode::BAD_REQUEST, "threshold must be between 0.0 and 1.0".to_string()));
    }
    let db = state.db.lock().await;
    let id_to_idx: HashMap<&str, usize> =
        db.records.iter().enumerate().map(|(i, r)| (r.id.as_str(), i)).collect();

    let mut seen_pairs: HashSet<(usize, usize)> = HashSet::new();
    let mut pairs: Vec<DedupPair> = Vec::new();

    for rec in &db.records {
        let raw = db.index.search(&rec.vector, 5, 20);
        for &(score, neighbor_id) in &raw {
            if neighbor_id == rec.id.as_str() || score < params.threshold {
                continue;
            }
            let i = id_to_idx[rec.id.as_str()];
            let j = id_to_idx[neighbor_id];
            let pair = (i.min(j), i.max(j));
            if seen_pairs.insert(pair) {
                pairs.push(DedupPair {
                    score,
                    a: RecordResponse::from(&db.records[pair.0]),
                    b: RecordResponse::from(&db.records[pair.1]),
                });
            }
        }
    }
    pairs.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    Ok(Json(pairs))
}

// ─── メイン ────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let api_key = std::env::var("OPENAI_API_KEY")
        .context("OPENAI_API_KEY is not set\nhint: export OPENAI_API_KEY=sk-...")?;

    match cli.command {
        // ── put ────────────────────────────────────────────────────────────
        Commands::Put { text, tags } => {
            println!("📥 generating embedding...");
            let vector = get_embedding(&text, &api_key).await?;
            let id = uuid::Uuid::new_v4().to_string();
            let mut records = read_db(&cli.file)?;
            records.push(IntentRecord {
                id: id.clone(),
                text: text.clone(),
                vector: vector.clone(),
                timestamp: now_secs(),
                tags: tags.clone(),
            });
            write_db(&cli.file, &records)?;
            let hp = hnsw_path(&cli.file);
            let mut index = hnsw::Hnsw::load(&hp)?;
            if index.len() != records.len() - 1 {
                index = hnsw::Hnsw::build(
                    records[..records.len() - 1]
                        .iter()
                        .map(|r| (r.id.clone(), r.vector.clone())),
                );
            }
            index.insert(id.clone(), vector);
            index.save(&hp)?;
            println!("✅ saved ({} total)", records.len());
            println!("   id:   {}", &id[..8]);
            println!("   text: {}", text);
            if !tags.is_empty() {
                println!("   tags: {}", tags.join(", "));
            }
        }

        // ── update ─────────────────────────────────────────────────────────
        Commands::Update { id, text, tags } => {
            let mut records = read_db(&cli.file)?;
            let target = records.iter().find(|r| r.id.starts_with(&id)).cloned();
            let Some(old) = target else {
                anyhow::bail!("no record found matching id \"{}\"", id);
            };

            println!("✏️  regenerating embedding...");
            let vector = get_embedding(&text, &api_key).await?;

            let new_tags = if tags.is_empty() { old.tags.clone() } else { tags };

            if let Some(rec) = records.iter_mut().find(|r| r.id.starts_with(&id)) {
                rec.text = text.clone();
                rec.vector = vector;
                rec.tags = new_tags.clone();
                rec.timestamp = now_secs();
            }
            write_db(&cli.file, &records)?;
            rebuild_and_save_hnsw(&cli.file, &records)?;

            println!("✅ updated");
            println!("   id:   {}...", &old.id[..8]);
            println!("   text: {} → {}", old.text, text);
            if !new_tags.is_empty() {
                println!("   tags: {}", new_tags.join(", "));
            }
        }

        // ── search ─────────────────────────────────────────────────────────
        Commands::Search { query, top, tags } => {
            let records = read_db(&cli.file)?;
            if records.is_empty() {
                println!("no records found. add one with `idb put \"your text\"`");
                return Ok(());
            }
            println!("🔍 searching for \"{}\"...", query);
            let query_vec = get_embedding(&query, &api_key).await?;
            let index = load_or_build_hnsw(&cli.file, &records)?;
            let record_map: HashMap<&str, &IntentRecord> =
                records.iter().map(|r| (r.id.as_str(), r)).collect();
            let raw = index.search(&query_vec, top * 4, 50);
            let scored: Vec<(f32, &IntentRecord)> = raw
                .iter()
                .filter_map(|&(score, id)| record_map.get(id).map(|rec| (score, *rec)))
                .filter(|(_, rec)| matches_tags(rec, &tags))
                .take(top)
                .collect();
            if !tags.is_empty() {
                println!("   tag filter: {}", tags.join(", "));
            }
            println!("\ntop {} results:", scored.len());
            println!("{}", "─".repeat(50));
            for (i, (score, rec)) in scored.iter().enumerate() {
                println!("{}. [score: {:.3}]{}", i + 1, score, format_tags(&rec.tags));
                println!("   {}", rec.text);
                println!("   ID: {}...", &rec.id[..8]);
                println!();
            }
        }

        // ── related ────────────────────────────────────────────────────────
        Commands::Related { id, top } => {
            let records = read_db(&cli.file)?;
            let target = records
                .iter()
                .find(|r| r.id.starts_with(&id))
                .ok_or_else(|| anyhow::anyhow!("no record found matching id \"{}\"", id))?;

            let index = load_or_build_hnsw(&cli.file, &records)?;
            let record_map: HashMap<&str, &IntentRecord> =
                records.iter().map(|r| (r.id.as_str(), r)).collect();

            // top+1 件取得して自分自身を除外
            let raw = index.search(&target.vector, top + 1, 50);
            let related: Vec<(f32, &IntentRecord)> = raw
                .iter()
                .filter_map(|&(score, rid)| record_map.get(rid).map(|rec| (score, *rec)))
                .filter(|(_, rec)| rec.id != target.id)
                .take(top)
                .collect();

            println!("🔗 related to [{}...] (top {})", &target.id[..8], related.len());
            println!("   origin: {}", target.text);
            println!("{}", "─".repeat(50));
            for (i, (score, rec)) in related.iter().enumerate() {
                println!("{}. [score: {:.3}]{}", i + 1, score, format_tags(&rec.tags));
                println!("   {}", rec.text);
                println!("   ID: {}...", &rec.id[..8]);
                println!();
            }
        }

        // ── list ───────────────────────────────────────────────────────────
        Commands::List { tags } => {
            let records = read_db(&cli.file)?;
            if records.is_empty() {
                println!("no records found.");
                return Ok(());
            }
            let filtered: Vec<&IntentRecord> =
                records.iter().filter(|rec| matches_tags(rec, &tags)).collect();
            if !tags.is_empty() {
                println!("📋 {} records (tag filter: {})", filtered.len(), tags.join(", "));
            } else {
                println!("📋 {} records", filtered.len());
            }
            println!("{}", "─".repeat(50));
            for (i, rec) in filtered.iter().enumerate() {
                println!(
                    "{}. [{}...]{} {}",
                    i + 1,
                    &rec.id[..8],
                    format_tags(&rec.tags),
                    rec.text
                );
            }
        }

        // ── delete ─────────────────────────────────────────────────────────
        Commands::Delete { id } => {
            let mut records = read_db(&cli.file)?;
            let before = records.len();
            records.retain(|r| !r.id.starts_with(&id));
            if records.len() == before {
                anyhow::bail!("no record found matching id \"{}\"", id);
            }
            write_db(&cli.file, &records)?;
            rebuild_and_save_hnsw(&cli.file, &records)?;
            println!("🗑️  deleted ({} remaining)", records.len());
        }

        // ── import ─────────────────────────────────────────────────────────
        Commands::Import { path, format } => {
            let fmt = format
                .unwrap_or_else(|| {
                    path.extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("txt")
                        .to_string()
                })
                .to_lowercase();

            let entries: Vec<ImportEntry> = match fmt.as_str() {
                "json" => {
                    let s = std::fs::read_to_string(&path)
                        .context("failed to read file")?;
                    serde_json::from_str(&s).context("invalid JSON format")?
                }
                "csv" => {
                    let mut rdr = csv::Reader::from_path(&path)
                        .context("failed to read CSV file")?;
                    let mut entries = Vec::new();
                    for result in rdr.records() {
                        let r = result?;
                        let text = r.get(0).unwrap_or("").trim().to_string();
                        if text.is_empty() {
                            continue;
                        }
                        let tags: Vec<String> = r
                            .get(1)
                            .unwrap_or("")
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                        entries.push(ImportEntry { text, tags });
                    }
                    entries
                }
                _ => {
                    // TXT: 1行1レコード、タグなし
                    let s = std::fs::read_to_string(&path)
                        .context("failed to read file")?;
                    s.lines()
                        .map(|l| l.trim().to_string())
                        .filter(|l| !l.is_empty())
                        .map(|text| ImportEntry { text, tags: vec![] })
                        .collect()
                }
            };

            if entries.is_empty() {
                println!("no entries to import.");
                return Ok(());
            }

            println!("📦 importing {} records...", entries.len());
            let mut records = read_db(&cli.file)?;
            let hp = hnsw_path(&cli.file);
            let mut index = load_or_build_hnsw(&cli.file, &records)?;
            let mut added = 0usize;

            for (i, entry) in entries.iter().enumerate() {
                let vector = get_embedding(&entry.text, &api_key).await?;
                let id = uuid::Uuid::new_v4().to_string();
                index.insert(id.clone(), vector.clone());
                records.push(IntentRecord {
                    id,
                    text: entry.text.clone(),
                    vector,
                    timestamp: now_secs(),
                    tags: entry.tags.clone(),
                });
                added += 1;
                if (i + 1) % 10 == 0 || i + 1 == entries.len() {
                    print!("\r   {}/{} done...", i + 1, entries.len());
                    let _ = std::io::stdout().flush();
                }
            }
            println!();
            write_db(&cli.file, &records)?;
            index.save(&hp)?;
            println!("✅ imported {} records ({} total)", added, records.len());
        }

        // ── export ─────────────────────────────────────────────────────────
        Commands::Export { output, format } => {
            let records = read_db(&cli.file)?;
            if records.is_empty() {
                println!("no records found.");
                return Ok(());
            }

            let content = match format.to_lowercase().as_str() {
                "csv" => {
                    let mut out = String::from("id,text,tags,timestamp\n");
                    for rec in &records {
                        // CSVの特殊文字をエスケープ
                        let text = rec.text.replace('"', "\"\"");
                        let tags = rec.tags.join(",").replace('"', "\"\"");
                        out.push_str(&format!(
                            "\"{}\",\"{}\",\"{}\",{}\n",
                            rec.id, text, tags, rec.timestamp
                        ));
                    }
                    out
                }
                _ => {
                    // json
                    let entries: Vec<ExportEntry> = records
                        .iter()
                        .map(|r| ExportEntry {
                            id: &r.id,
                            text: &r.text,
                            tags: &r.tags,
                            timestamp: r.timestamp,
                        })
                        .collect();
                    serde_json::to_string_pretty(&entries)?
                }
            };

            if let Some(out_path) = output {
                std::fs::write(&out_path, &content)?;
                println!("✅ exported {} records to {}", records.len(), out_path.display());
            } else {
                println!("{}", content);
            }
        }

        // ── dedup ──────────────────────────────────────────────────────────
        Commands::Dedup { threshold, delete } => {
            let records = read_db(&cli.file)?;
            if records.len() < 2 {
                println!("need at least 2 records.");
                return Ok(());
            }

            println!("🔎 detecting duplicates (threshold: {:.2})...", threshold);
            let index = load_or_build_hnsw(&cli.file, &records)?;
            let id_to_idx: HashMap<&str, usize> =
                records.iter().enumerate().map(|(i, r)| (r.id.as_str(), i)).collect();

            let mut seen_pairs: HashSet<(usize, usize)> = HashSet::new();
            let mut dup_pairs: Vec<(usize, usize, f32)> = Vec::new();

            for rec in &records {
                let raw = index.search(&rec.vector, 5, 20);
                for &(score, neighbor_id) in &raw {
                    if neighbor_id == rec.id.as_str() {
                        continue;
                    }
                    if score < threshold {
                        continue;
                    }
                    let i = id_to_idx[rec.id.as_str()];
                    let j = id_to_idx[neighbor_id];
                    let pair = (i.min(j), i.max(j));
                    if seen_pairs.insert(pair) {
                        dup_pairs.push((pair.0, pair.1, score));
                    }
                }
            }

            if dup_pairs.is_empty() {
                println!("✅ no duplicates found.");
                return Ok(());
            }

            println!("\n⚠️  {} duplicate pair(s) found:", dup_pairs.len());
            println!("{}", "─".repeat(50));
            for (i, j, score) in &dup_pairs {
                let a = &records[*i];
                let b = &records[*j];
                println!("[score: {:.3}]", score);
                println!("  A [{}...] {}", &a.id[..8], a.text);
                println!("  B [{}...] {}", &b.id[..8], b.text);
                println!();
            }

            if delete {
                // 各ペアでより新しいほう（大きいtimestamp）を削除
                let to_delete: HashSet<usize> = dup_pairs
                    .iter()
                    .map(|&(i, j, _)| {
                        if records[i].timestamp >= records[j].timestamp { i } else { j }
                    })
                    .collect();

                let remaining: Vec<IntentRecord> = records
                    .into_iter()
                    .enumerate()
                    .filter(|(i, _)| !to_delete.contains(i))
                    .map(|(_, r)| r)
                    .collect();

                write_db(&cli.file, &remaining)?;
                rebuild_and_save_hnsw(&cli.file, &remaining)?;
                println!("🗑️  deleted {} record(s) ({} remaining)", to_delete.len(), remaining.len());
            } else {
                println!("run with --delete to remove duplicates automatically.");
            }
        }

        // ── serve ──────────────────────────────────────────────────────────
        Commands::Serve { port, host } => {
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
                .route("/records/:id", axum::routing::patch(handle_update))
                .route("/records/:id/related", get(handle_related))
                .route("/search", get(handle_search))
                .route("/dedup", get(handle_dedup))
                .with_state(state);

            let addr = format!("{}:{}", host, port);
            let listener = tokio::net::TcpListener::bind(&addr).await?;
            println!("🚀 IntentDB API server started");
            println!("   http://{}:{}", host, port);
            println!("   db file: {}", cli.file.display());
            println!();
            println!("endpoints:");
            println!("  POST   /records              add a record");
            println!("  GET    /records              list records (?tag=xxx)");
            println!("  PATCH  /records/:id          update a record");
            println!("  DELETE /records/:id          delete a record");
            println!("  GET    /records/:id/related  related records (?top=5)");
            println!("  GET    /search               search (?q=xxx&top=5&tag=xxx)");
            println!("  GET    /dedup                detect duplicates (?threshold=0.95)");
            axum::serve(listener, app).await?;
        }
    }

    Ok(())
}
