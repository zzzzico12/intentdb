mod hnsw;
mod mcp;

use anyhow::{Context, Result};
use axum::{
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    response::{Html, Json, IntoResponse, Response},
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

// ─── CLI ──────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "idb")]
#[command(about = "IntentDB — schema-free, intent-native storage engine")]
struct Cli {
    /// DB file path
    #[arg(short, long, default_value = "data.idb")]
    file: PathBuf,

    /// Namespace — stores data in <ns>.idb next to --file (overrides file stem)
    #[arg(long, default_value = "")]
    ns: String,

    /// Embedding API endpoint (OpenAI-compatible). Ollama: http://localhost:11434/v1/embeddings
    #[arg(long, env = "IDB_EMBEDDING_URL",
          default_value = "https://api.openai.com/v1/embeddings")]
    embedding_url: String,

    /// Embedding model. Ollama example: nomic-embed-text
    #[arg(long, env = "IDB_EMBEDDING_MODEL", default_value = "text-embedding-3-small")]
    embedding_model: String,

    /// LLM API endpoint for `ask` (OpenAI-compatible). Ollama: http://localhost:11434/v1/chat/completions
    #[arg(long, env = "IDB_LLM_URL",
          default_value = "https://api.openai.com/v1/chat/completions")]
    llm_url: String,

    /// LLM model for `ask`. Ollama example: llama3
    #[arg(long, env = "IDB_LLM_MODEL", default_value = "gpt-4o-mini")]
    llm_model: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Add a record
    Put {
        text: String,
        #[arg(short, long = "tag")]
        tags: Vec<String>,
    },
    /// Update an existing record (id prefix)
    Update {
        id: String,
        text: String,
        #[arg(short, long = "tag")]
        tags: Vec<String>,
    },
    /// Semantic (or hybrid) search
    Search {
        query: String,
        #[arg(short, long, default_value = "5")]
        top: usize,
        #[arg(short, long = "tag")]
        tags: Vec<String>,
        /// Only records added before this date (YYYY-MM-DD or Unix timestamp)
        #[arg(long)]
        before: Option<String>,
        /// Only records added after this date (YYYY-MM-DD or Unix timestamp)
        #[arg(long)]
        after: Option<String>,
        /// Hybrid weight: 1.0 = pure semantic, 0.0 = pure keyword
        #[arg(long, default_value = "1.0")]
        alpha: f32,
        /// Minimum similarity score threshold (0.0–1.0)
        #[arg(long, default_value = "0.0")]
        min_score: f32,
    },
    /// Ask a question answered from stored records (RAG)
    Ask {
        question: String,
        /// Number of source records to use as context
        #[arg(short, long, default_value = "5")]
        top: usize,
    },
    /// Summarize stored records via LLM
    Summarize {
        /// Focus topic (optional, e.g. "billing issues this week")
        topic: Option<String>,
        #[arg(short, long = "tag")]
        tags: Vec<String>,
        /// Only records added before this date
        #[arg(long)]
        before: Option<String>,
        /// Only records added after this date
        #[arg(long)]
        after: Option<String>,
        /// Max records to include as context
        #[arg(short, long, default_value = "20")]
        top: usize,
    },
    /// Cluster records by semantic similarity (k-means)
    Cluster {
        /// Number of clusters
        #[arg(short, long, default_value = "5")]
        k: usize,
        #[arg(short, long = "tag")]
        tags: Vec<String>,
    },
    /// Find semantically related records by ID
    Related {
        id: String,
        #[arg(short, long, default_value = "5")]
        top: usize,
    },
    /// List all records
    List {
        #[arg(short, long = "tag")]
        tags: Vec<String>,
    },
    /// Show prompts and Claude responses interleaved chronologically
    Timeline {
        /// Filter by session ID prefix
        #[arg(long)]
        session: Option<String>,
        /// Maximum entries to display
        #[arg(short, long)]
        limit: Option<usize>,
        /// Also show Note-type records (raw text, neither prompt nor response)
        #[arg(long)]
        show_notes: bool,
    },
    /// Delete a record (id prefix)
    Delete { id: String },
    /// Bulk import from JSON / CSV / TXT
    Import {
        path: PathBuf,
        #[arg(short, long)]
        format: Option<String>,
    },
    /// Export records to JSON or CSV (vectors excluded)
    Export {
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(short, long, default_value = "json")]
        format: String,
    },
    /// Detect duplicate records
    Dedup {
        #[arg(long, default_value = "0.95")]
        threshold: f32,
        #[arg(long)]
        delete: bool,
    },
    /// Start HTTP API server
    Serve {
        #[arg(short, long, default_value = "3000")]
        port: u16,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
    },
    /// Start MCP stdio server (for Claude Code / AI assistant integration)
    Mcp,
    /// Sync records with a remote intentdb server
    Sync {
        #[command(subcommand)]
        action: SyncAction,
    },
}

#[derive(Subcommand)]
enum SyncAction {
    /// Pull records from a remote server (adds records missing locally)
    Pull {
        /// Remote intentdb server URL, e.g. http://192.168.1.10:3000
        #[arg(long)]
        from: String,
    },
    /// Push local records to a remote server (adds records missing remotely)
    Push {
        /// Remote intentdb server URL, e.g. http://192.168.1.10:3000
        #[arg(long)]
        to: String,
    },
}

// ─── Data structures ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Debug, Clone)]
struct IntentRecord {
    id: String,
    text: String,
    vector: Vec<f32>,
    timestamp: u64,
    tags: Vec<String>,
}

#[derive(Deserialize)]
struct ImportEntry {
    text: String,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Serialize)]
struct ExportEntry<'a> {
    id: &'a str,
    text: &'a str,
    tags: &'a Vec<String>,
    timestamp: u64,
}

// ─── .idb file format ─────────────────────────────────────────────────────────
//
// [MAGIC: 4B "IDB2"][record count: u32]
// Per record:
//   [id length: u16][id bytes]
//   [text length: u32][text bytes]
//   [vector dims: u32][f32 × N]
//   [timestamp: u64]
//   [tag count: u16][[tag length: u16][tag bytes] × N]

const MAGIC_V2: &[u8; 4] = b"IDB2";
const MAGIC_V1: &[u8; 4] = b"IDB1";

const MAX_TEXT_BYTES: usize = 10 * 1024 * 1024;
const MAX_VECTOR_DIM: usize = 16_384;
const MAX_TAG_COUNT: usize = 1_024;
const MAX_TAG_BYTES: usize = 4_096;
const MAX_RECORDS: usize = 10_000_000;
const MAX_INPUT_TEXT: usize = 32_768;
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

// ─── HNSW helpers ────────────────────────────────────────────────────────────

fn hnsw_path(db_path: &Path) -> PathBuf {
    db_path.with_extension("hnsw")
}

/// Resolve effective DB file path, applying namespace if set.
fn effective_file(file: &Path, ns: &str) -> PathBuf {
    if ns.is_empty() {
        file.to_path_buf()
    } else {
        let dir = file.parent().unwrap_or(Path::new("."));
        dir.join(format!("{}.idb", ns))
    }
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
    let index = hnsw::Hnsw::build(records.iter().map(|r| (r.id.clone(), r.vector.clone())));
    index.save(&hp)?;
    Ok(index)
}

fn rebuild_and_save_hnsw(db_path: &Path, records: &[IntentRecord]) -> Result<hnsw::Hnsw> {
    let index = hnsw::Hnsw::build(records.iter().map(|r| (r.id.clone(), r.vector.clone())));
    index.save(&hnsw_path(db_path))?;
    Ok(index)
}

// ─── General helpers ─────────────────────────────────────────────────────────

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

/// Format a Unix timestamp (seconds) as "YYYY-MM-DD HH:MM:SS" UTC.
fn format_ts(ts: u64) -> String {
    let secs_in_day = 86400u64;
    let time_of_day = ts % secs_in_day;
    let days = ts / secs_in_day;
    let hh = time_of_day / 3600;
    let mm = (time_of_day % 3600) / 60;
    let ss = time_of_day % 60;
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, m, d, hh, mm, ss)
}

/// Truncate a string to max_chars Unicode characters, appending '…' if truncated.
fn truncate(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{}…", prefix)
    } else {
        prefix
    }
}

pub enum TimelineEntry {
    User { prompt: String, session_id: Option<String> },
    Claude { text: String, session_id: Option<String> },
    Note { text: String },
}

pub fn classify_record(rec: &IntentRecord) -> TimelineEntry {
    if rec.tags.iter().any(|t| t == "response") {
        // New format: JSON with hook_event_name == "Stop"
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&rec.text) {
            if v.get("hook_event_name").and_then(|h| h.as_str()) == Some("Stop") {
                let text = v.get("response").and_then(|r| r.as_str()).unwrap_or("").to_string();
                let session_id = v.get("session_id").and_then(|s| s.as_str()).map(|s| s.to_string());
                return TimelineEntry::Claude { text, session_id };
            }
        }
        // Legacy format: plain text
        return TimelineEntry::Claude { text: rec.text.clone(), session_id: None };
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&rec.text) {
        if v.get("hook_event_name").and_then(|h| h.as_str()) == Some("UserPromptSubmit") {
            let prompt = v.get("prompt").and_then(|p| p.as_str()).unwrap_or("<no prompt>").to_string();
            let session_id = v.get("session_id").and_then(|s| s.as_str()).map(|s| s.to_string());
            return TimelineEntry::User { prompt, session_id };
        }
    }
    TimelineEntry::Note { text: rec.text.clone() }
}

/// Fraction of query tokens found in text (case-insensitive).
fn keyword_score(text: &str, query_words: &[String]) -> f32 {
    if query_words.is_empty() {
        return 0.0;
    }
    let text_lower = text.to_lowercase();
    let matched = query_words.iter().filter(|w| text_lower.contains(w.as_str())).count();
    matched as f32 / query_words.len() as f32
}

/// Split a query string into lowercase tokens (length > 1).
fn tokenize(query: &str) -> Vec<String> {
    query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 1)
        .map(|w| w.to_lowercase())
        .collect()
}

/// Parse YYYY-MM-DD or Unix timestamp into seconds since epoch.
fn parse_date(s: &str) -> Result<u64> {
    if let Ok(ts) = s.parse::<u64>() {
        return Ok(ts);
    }
    let parts: Vec<&str> = s.splitn(3, '-').collect();
    if parts.len() == 3 {
        let y: i64 = parts[0].parse().context("invalid year")?;
        let m: i64 = parts[1].parse().context("invalid month")?;
        let d: i64 = parts[2].parse().context("invalid day")?;
        let a = (14 - m) / 12;
        let yr = y + 4800 - a;
        let mo = m + 12 * a - 3;
        let jdn = d + (153 * mo + 2) / 5 + 365 * yr + yr / 4 - yr / 100 + yr / 400 - 32045;
        let epoch_jdn: i64 = 2440588;
        let days = jdn - epoch_jdn;
        anyhow::ensure!(days >= 0, "date before Unix epoch: {}", s);
        return Ok(days as u64 * 86400);
    }
    anyhow::bail!("invalid date (use YYYY-MM-DD or Unix timestamp): {}", s)
}

// ─── Embedding API (OpenAI-compatible) ───────────────────────────────────────

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

async fn get_embedding(text: &str, api_key: &str, url: &str, model: &str) -> Result<Vec<f32>> {
    let client = reqwest::Client::new();
    let req = EmbedRequest { input: text.to_string(), model: model.to_string() };
    let resp: EmbedResponse = client
        .post(url)
        .bearer_auth(api_key)
        .json(&req)
        .send()
        .await
        .context("failed to connect to embedding API")?
        .json()
        .await
        .context("failed to parse embedding API response")?;
    resp.data
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("embedding API returned empty data"))
        .map(|d| d.embedding)
}

// ─── Chat API (OpenAI-compatible, for ask command) ────────────────────────────

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
}

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessageContent,
}

#[derive(Deserialize)]
struct ChatMessageContent {
    content: String,
}

async fn ask_llm(
    question: &str,
    context: &str,
    url: &str,
    model: &str,
    api_key: &str,
) -> Result<String> {
    let client = reqwest::Client::new();
    let system = "You are a helpful assistant. Answer the question based solely on the \
                  provided context. If the answer is not in the context, say so clearly.";
    let user_content = format!("Context:\n{}\n\nQuestion: {}", context, question);
    let req = ChatRequest {
        model: model.to_string(),
        messages: vec![
            ChatMessage { role: "system".to_string(), content: system.to_string() },
            ChatMessage { role: "user".to_string(), content: user_content },
        ],
    };
    let resp: ChatResponse = client
        .post(url)
        .bearer_auth(api_key)
        .json(&req)
        .send()
        .await
        .context("failed to connect to LLM API")?
        .json()
        .await
        .context("failed to parse LLM API response")?;
    resp.choices
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("LLM returned empty response"))
        .map(|c| c.message.content)
}

// ─── K-means clustering ───────────────────────────────────────────────────────

fn dot_sim(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 { 0.0 } else { dot / (na * nb) }
}

/// K-means clustering using cosine similarity. Returns cluster index per record.
fn kmeans(vecs: &[Vec<f32>], k: usize) -> Vec<usize> {
    if vecs.is_empty() || k == 0 {
        return vec![];
    }
    let k = k.min(vecs.len());
    let dim = vecs[0].len();
    // Spread initial centroids evenly
    let mut centroids: Vec<Vec<f32>> =
        (0..k).map(|i| vecs[i * vecs.len() / k].clone()).collect();
    let mut assignments = vec![0usize; vecs.len()];

    for _ in 0..30 {
        // Assign each vector to the nearest centroid
        let mut changed = false;
        for (i, v) in vecs.iter().enumerate() {
            let best = centroids
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| {
                    dot_sim(v, a)
                        .partial_cmp(&dot_sim(v, b))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(ci, _)| ci)
                .unwrap_or(0);
            if assignments[i] != best {
                assignments[i] = best;
                changed = true;
            }
        }
        if !changed {
            break;
        }
        // Recompute centroids as mean of members
        for (c, centroid) in centroids.iter_mut().enumerate() {
            let members: Vec<&Vec<f32>> = vecs
                .iter()
                .enumerate()
                .filter(|(i, _)| assignments[*i] == c)
                .map(|(_, v)| v)
                .collect();
            if members.is_empty() {
                continue;
            }
            let mut new_centroid = vec![0f32; dim];
            for m in &members {
                for (j, &val) in m.iter().enumerate() {
                    new_centroid[j] += val;
                }
            }
            let n = members.len() as f32;
            for val in &mut new_centroid {
                *val /= n;
            }
            *centroid = new_centroid;
        }
    }
    assignments
}

// ─── HTTP API state & types ───────────────────────────────────────────────────

struct DbState {
    records: Vec<IntentRecord>,
    index: hnsw::Hnsw,
}

struct AppState {
    db_path: PathBuf,
    hnsw_path: PathBuf,
    api_key: String,
    embedding_url: String,
    embedding_model: String,
    llm_url: String,
    llm_model: String,
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

#[derive(Serialize, Clone)]
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
    before: Option<u64>,
    after: Option<u64>,
    #[serde(default = "default_alpha")]
    alpha: f32,
    #[serde(default)]
    min_score: f32,
}

#[derive(Deserialize)]
struct SummarizeQuery {
    topic: Option<String>,
    #[serde(default)]
    tag: Vec<String>,
    before: Option<u64>,
    after: Option<u64>,
    #[serde(default = "default_summarize_top")]
    top: usize,
}

#[derive(Serialize)]
struct SummarizeResponse {
    summary: String,
    record_count: usize,
}

fn default_summarize_top() -> usize { 20 }

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
fn default_alpha() -> f32 { 1.0 }

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

#[derive(Deserialize)]
struct AskBody {
    question: String,
    #[serde(default = "default_top")]
    top: usize,
}

#[derive(Serialize)]
struct AskResponse {
    answer: String,
    sources: Vec<RecordResponse>,
}

type AppError = (StatusCode, String);

fn internal(e: anyhow::Error) -> AppError {
    let msg = format!("{:#}", e);
    eprintln!("internal error: {}", msg);
    // Surface actionable messages to the client
    if msg.contains("missing field `data`") || msg.contains("parse embedding") || msg.contains("parse LLM") {
        return (
            StatusCode::BAD_GATEWAY,
            "Embedding API error — check that OPENAI_API_KEY is set, or configure --embedding-url / --llm-url for Ollama".to_string(),
        );
    }
    if msg.contains("connect to embedding") || msg.contains("connect to LLM") || msg.contains("Connection refused") {
        return (
            StatusCode::BAD_GATEWAY,
            "Cannot reach embedding/LLM API — is OPENAI_API_KEY set? For Ollama, is `ollama serve` running?".to_string(),
        );
    }
    (StatusCode::INTERNAL_SERVER_ERROR, msg)
}

// ─── Input validation ─────────────────────────────────────────────────────────

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

// ─── HTTP handlers ────────────────────────────────────────────────────────────

async fn handle_put(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PutBody>,
) -> Result<Json<PutResponse>, AppError> {
    validate_text(&body.text)?;
    validate_tags(&body.tags)?;
    let vector = get_embedding(&body.text, &state.api_key, &state.embedding_url, &state.embedding_model)
        .await.map_err(internal)?;
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
    let result = db.records.iter()
        .filter(|r| matches_tags(r, &filter.tag))
        .map(RecordResponse::from)
        .collect();
    Ok(Json(result))
}

async fn handle_timeline(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let db = state.db.lock().await;
    let mut sorted: Vec<&IntentRecord> = db.records.iter().collect();
    sorted.sort_by_key(|r| r.timestamp);
    let result = sorted.into_iter().filter_map(|rec| {
        match classify_record(rec) {
            TimelineEntry::User { prompt, session_id } => Some(serde_json::json!({
                "role": "user",
                "timestamp": rec.timestamp,
                "session_id": session_id,
                "text": prompt,
                "id": rec.id,
            })),
            TimelineEntry::Claude { text, session_id } => Some(serde_json::json!({
                "role": "claude",
                "timestamp": rec.timestamp,
                "session_id": session_id,
                "text": text,
                "id": rec.id,
            })),
            TimelineEntry::Note { .. } => None,
        }
    }).collect();
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
    if !(0.0..=1.0).contains(&params.alpha) {
        return Err((StatusCode::BAD_REQUEST, "alpha must be between 0.0 and 1.0".to_string()));
    }
    let query_vec = get_embedding(&params.q, &state.api_key, &state.embedding_url, &state.embedding_model)
        .await.map_err(internal)?;
    let query_words = tokenize(&params.q);
    let db = state.db.lock().await;
    let record_map: HashMap<&str, &IntentRecord> =
        db.records.iter().map(|r| (r.id.as_str(), r)).collect();
    let raw = db.index.search(&query_vec, params.top * 4, 50);
    let mut scored: Vec<(f32, &IntentRecord)> = raw
        .iter()
        .filter_map(|&(sem, id)| record_map.get(id).map(|rec| (sem, *rec)))
        .filter(|(_, rec)| matches_tags(rec, &params.tag))
        .filter(|(_, rec)| params.before.is_none_or(|b| rec.timestamp < b))
        .filter(|(_, rec)| params.after.is_none_or(|a| rec.timestamp >= a))
        .map(|(sem, rec)| {
            let score = if params.alpha >= 1.0 {
                sem
            } else {
                let kw = keyword_score(&rec.text, &query_words);
                params.alpha * sem + (1.0 - params.alpha) * kw
            };
            (score, rec)
        })
        .filter(|(score, _)| *score >= params.min_score)
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let result = scored.iter().take(params.top).map(|(score, rec)| SearchResult {
        score: *score,
        id: rec.id.clone(),
        text: rec.text.clone(),
        tags: rec.tags.clone(),
        timestamp: rec.timestamp,
    }).collect();
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
    db.index = hnsw::Hnsw::build(db.records.iter().map(|r| (r.id.clone(), r.vector.clone())));
    write_db(&state.db_path, &db.records).map_err(internal)?;
    db.index.save(&state.hnsw_path).map_err(internal)?;
    let remaining = db.records.len();
    Ok(Json(serde_json::json!({ "deleted": deleted, "remaining": remaining })))
}

async fn handle_update(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<UpdateBody>,
) -> Result<Json<RecordResponse>, AppError> {
    validate_text(&body.text)?;
    validate_tags(&body.tags)?;
    let vector = get_embedding(&body.text, &state.api_key, &state.embedding_url, &state.embedding_model)
        .await.map_err(internal)?;
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

async fn handle_ask(
    State(state): State<Arc<AppState>>,
    Json(body): Json<AskBody>,
) -> Result<Json<AskResponse>, AppError> {
    validate_text(&body.question)?;
    let top = body.top.clamp(1, 20);
    let query_vec = get_embedding(&body.question, &state.api_key, &state.embedding_url, &state.embedding_model)
        .await.map_err(internal)?;
    let db = state.db.lock().await;
    let record_map: HashMap<&str, &IntentRecord> =
        db.records.iter().map(|r| (r.id.as_str(), r)).collect();
    let raw = db.index.search(&query_vec, top * 2, 50);
    let sources: Vec<&IntentRecord> = raw
        .iter()
        .filter_map(|&(_, id)| record_map.get(id).copied())
        .take(top)
        .collect();
    let context = sources.iter().enumerate()
        .map(|(i, r)| format!("[{}] {}", i + 1, r.text))
        .collect::<Vec<_>>()
        .join("\n");
    let source_responses: Vec<RecordResponse> = sources.iter().map(|r| RecordResponse::from(*r)).collect();
    drop(db);
    let answer = ask_llm(&body.question, &context, &state.llm_url, &state.llm_model, &state.api_key)
        .await.map_err(internal)?;
    Ok(Json(AskResponse { answer, sources: source_responses }))
}

async fn handle_ui() -> Html<&'static str> {
    Html(include_str!("../ui/index.html"))
}

async fn handle_favicon() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

async fn handle_export(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<IntentRecord>> {
    let db = state.db.lock().await;
    Json(db.records.clone())
}

#[derive(Deserialize)]
struct ImportBody {
    records: Vec<IntentRecord>,
}

#[derive(Serialize)]
struct ImportResponse {
    added: usize,
    total: usize,
}

async fn handle_import(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ImportBody>,
) -> Result<Json<ImportResponse>, AppError> {
    let mut db = state.db.lock().await;
    let existing: HashSet<String> = db.records.iter().map(|r| r.id.clone()).collect();
    let new_records: Vec<IntentRecord> = body.records
        .into_iter()
        .filter(|r| !existing.contains(&r.id))
        .collect();
    let added = new_records.len();
    for rec in new_records {
        db.index.insert(rec.id.clone(), rec.vector.clone());
        db.records.push(rec);
    }
    let total = db.records.len();
    write_db(&state.db_path, &db.records).map_err(internal)?;
    db.index.save(&state.hnsw_path).map_err(internal)?;
    Ok(Json(ImportResponse { added, total }))
}

async fn handle_summarize(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SummarizeQuery>,
) -> Result<Json<SummarizeResponse>, AppError> {
    if params.top == 0 || params.top > 200 {
        return Err((StatusCode::BAD_REQUEST, "top must be between 1 and 200".to_string()));
    }
    let db = state.db.lock().await;
    let records: Vec<&IntentRecord> = db.records.iter()
        .filter(|r| matches_tags(r, &params.tag))
        .filter(|r| params.before.is_none_or(|b| r.timestamp < b))
        .filter(|r| params.after.is_none_or(|a| r.timestamp >= a))
        .take(params.top)
        .collect();
    if records.is_empty() {
        return Ok(Json(SummarizeResponse {
            summary: "No records found matching the given filters.".to_string(),
            record_count: 0,
        }));
    }
    let record_count = records.len();
    let context = records.iter().enumerate()
        .map(|(i, r)| format!("[{}] {}", i + 1, r.text))
        .collect::<Vec<_>>()
        .join("\n");
    drop(db);
    let topic_line = params.topic.as_deref().unwrap_or("the stored records");
    let prompt = format!(
        "Summarize the following records about {}. \
         Identify key themes, patterns, and notable items. \
         Be concise but comprehensive.",
        topic_line
    );
    let answer = ask_llm(&prompt, &context, &state.llm_url, &state.llm_model, &state.api_key)
        .await.map_err(internal)?;
    Ok(Json(SummarizeResponse { summary: answer, record_count }))
}

// ─── main ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // OPENAI_API_KEY is optional — not needed when using local Ollama endpoints
    let api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();

    let db_file = effective_file(&cli.file, &cli.ns);

    match cli.command {
        // ── put ────────────────────────────────────────────────────────────
        Commands::Put { text, tags } => {
            println!("📥 generating embedding...");
            let vector = get_embedding(&text, &api_key, &cli.embedding_url, &cli.embedding_model).await?;
            let id = uuid::Uuid::new_v4().to_string();
            let mut records = read_db(&db_file)?;
            records.push(IntentRecord {
                id: id.clone(),
                text: text.clone(),
                vector: vector.clone(),
                timestamp: now_secs(),
                tags: tags.clone(),
            });
            write_db(&db_file, &records)?;
            let hp = hnsw_path(&db_file);
            let mut index = hnsw::Hnsw::load(&hp)?;
            if index.len() != records.len() - 1 {
                index = hnsw::Hnsw::build(
                    records[..records.len() - 1].iter().map(|r| (r.id.clone(), r.vector.clone())),
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
            if !cli.ns.is_empty() {
                println!("   ns:   {}", cli.ns);
            }
        }

        // ── update ─────────────────────────────────────────────────────────
        Commands::Update { id, text, tags } => {
            let mut records = read_db(&db_file)?;
            let target = records.iter().find(|r| r.id.starts_with(&id)).cloned();
            let Some(old) = target else {
                anyhow::bail!("no record found matching id \"{}\"", id);
            };
            println!("✏️  regenerating embedding...");
            let vector = get_embedding(&text, &api_key, &cli.embedding_url, &cli.embedding_model).await?;
            let new_tags = if tags.is_empty() { old.tags.clone() } else { tags };
            if let Some(rec) = records.iter_mut().find(|r| r.id.starts_with(&id)) {
                rec.text = text.clone();
                rec.vector = vector;
                rec.tags = new_tags.clone();
                rec.timestamp = now_secs();
            }
            write_db(&db_file, &records)?;
            rebuild_and_save_hnsw(&db_file, &records)?;
            println!("✅ updated");
            println!("   id:   {}...", &old.id[..8]);
            println!("   text: {} → {}", old.text, text);
            if !new_tags.is_empty() {
                println!("   tags: {}", new_tags.join(", "));
            }
        }

        // ── search ─────────────────────────────────────────────────────────
        Commands::Search { query, top, tags, before, after, alpha, min_score } => {
            let records = read_db(&db_file)?;
            if records.is_empty() {
                println!("no records found. add one with `idb put \"your text\"`");
                return Ok(());
            }
            if !(0.0..=1.0).contains(&alpha) {
                anyhow::bail!("alpha must be between 0.0 and 1.0");
            }
            let before_ts = before.as_deref().map(parse_date).transpose()?;
            let after_ts = after.as_deref().map(parse_date).transpose()?;

            println!("🔍 searching for \"{}\"...", query);
            let query_vec = get_embedding(&query, &api_key, &cli.embedding_url, &cli.embedding_model).await?;
            let query_words = tokenize(&query);
            let index = load_or_build_hnsw(&db_file, &records)?;
            let record_map: HashMap<&str, &IntentRecord> =
                records.iter().map(|r| (r.id.as_str(), r)).collect();
            let raw = index.search(&query_vec, top * 4, 50);
            let mut scored: Vec<(f32, &IntentRecord)> = raw
                .iter()
                .filter_map(|&(sem, id)| record_map.get(id).map(|rec| (sem, *rec)))
                .filter(|(_, rec)| matches_tags(rec, &tags))
                .filter(|(_, rec)| before_ts.is_none_or(|b| rec.timestamp < b))
                .filter(|(_, rec)| after_ts.is_none_or(|a| rec.timestamp >= a))
                .map(|(sem, rec)| {
                    let score = if alpha >= 1.0 {
                        sem
                    } else {
                        let kw = keyword_score(&rec.text, &query_words);
                        alpha * sem + (1.0 - alpha) * kw
                    };
                    (score, rec)
                })
                .filter(|(score, _)| *score >= min_score)
                .collect();
            scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

            if !tags.is_empty() {
                println!("   tag filter: {}", tags.join(", "));
            }
            if before_ts.is_some() || after_ts.is_some() {
                println!("   time filter: {} ~ {}",
                    after.as_deref().unwrap_or("*"),
                    before.as_deref().unwrap_or("*"));
            }
            if alpha < 1.0 {
                println!("   hybrid: alpha={:.2} (semantic) + {:.2} (keyword)", alpha, 1.0 - alpha);
            }
            println!("\ntop {} results:", scored.len().min(top));
            println!("{}", "─".repeat(50));
            for (i, (score, rec)) in scored.iter().take(top).enumerate() {
                println!("{}. [score: {:.3}]{}", i + 1, score, format_tags(&rec.tags));
                println!("   {}", rec.text);
                println!("   id: {}...", &rec.id[..8]);
                println!();
            }
        }

        // ── ask ────────────────────────────────────────────────────────────
        Commands::Ask { question, top } => {
            let records = read_db(&db_file)?;
            if records.is_empty() {
                println!("no records found. add some with `idb put \"your text\"`");
                return Ok(());
            }
            println!("🤔 finding relevant context...");
            let query_vec = get_embedding(&question, &api_key, &cli.embedding_url, &cli.embedding_model).await?;
            let index = load_or_build_hnsw(&db_file, &records)?;
            let record_map: HashMap<&str, &IntentRecord> =
                records.iter().map(|r| (r.id.as_str(), r)).collect();
            let raw = index.search(&query_vec, top * 2, 50);
            let sources: Vec<&IntentRecord> = raw
                .iter()
                .filter_map(|&(_, id)| record_map.get(id).copied())
                .take(top)
                .collect();
            let context = sources.iter().enumerate()
                .map(|(i, r)| format!("[{}] {}", i + 1, r.text))
                .collect::<Vec<_>>()
                .join("\n");
            println!("💬 asking {} ({} sources)...\n", cli.llm_model, sources.len());
            let answer = ask_llm(&question, &context, &cli.llm_url, &cli.llm_model, &api_key).await?;
            println!("{}\n", answer);
            println!("{}", "─".repeat(50));
            println!("sources:");
            for (i, rec) in sources.iter().enumerate() {
                println!("  {}. [{}...]{} {}", i + 1, &rec.id[..8], format_tags(&rec.tags), rec.text);
            }
        }

        // ── related ────────────────────────────────────────────────────────
        Commands::Related { id, top } => {
            let records = read_db(&db_file)?;
            let target = records.iter().find(|r| r.id.starts_with(&id))
                .ok_or_else(|| anyhow::anyhow!("no record found matching id \"{}\"", id))?;
            let index = load_or_build_hnsw(&db_file, &records)?;
            let record_map: HashMap<&str, &IntentRecord> =
                records.iter().map(|r| (r.id.as_str(), r)).collect();
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
                println!("   id: {}...", &rec.id[..8]);
                println!();
            }
        }

        // ── list ───────────────────────────────────────────────────────────
        Commands::List { tags } => {
            let records = read_db(&db_file)?;
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
            if !cli.ns.is_empty() {
                println!("   namespace: {}", cli.ns);
            }
            println!("{}", "─".repeat(50));
            for (i, rec) in filtered.iter().enumerate() {
                println!("{}. [{}...]{} {}", i + 1, &rec.id[..8], format_tags(&rec.tags), rec.text);
            }
        }

        // ── timeline ───────────────────────────────────────────────────────
        Commands::Timeline { session, limit, show_notes } => {
            let records = read_db(&db_file)?;
            if records.is_empty() {
                println!("no records found.");
                return Ok(());
            }
            let mut sorted: Vec<&IntentRecord> = records.iter().collect();
            sorted.sort_by_key(|r| r.timestamp);

            let entries: Vec<(&IntentRecord, TimelineEntry)> = sorted
                .into_iter()
                .filter_map(|rec| {
                    let entry = classify_record(rec);
                    if let Some(ref sid) = session {
                        let keep = match &entry {
                            TimelineEntry::User { session_id: Some(s), .. } => s.starts_with(sid.as_str()),
                            TimelineEntry::User { session_id: None, .. } => false,
                            TimelineEntry::Claude { session_id: Some(s), .. } => s.starts_with(sid.as_str()),
                            TimelineEntry::Claude { session_id: None, .. } => true, // legacy: show anyway
                            TimelineEntry::Note { .. } => false,
                        };
                        if !keep { return None; }
                    }
                    match &entry {
                        TimelineEntry::Note { .. } if !show_notes => None,
                        _ => Some((rec, entry)),
                    }
                })
                .collect();

            let take_n = limit.unwrap_or(entries.len());
            println!("Timeline ({} entries{})",
                entries.len(),
                session.as_deref().map(|s| format!(", session: {}", s)).unwrap_or_default());
            println!("{}", "─".repeat(60));
            for (rec, entry) in entries.iter().take(take_n) {
                let ts = format_ts(rec.timestamp);
                match entry {
                    TimelineEntry::User { prompt, .. } => {
                        println!("[{}] \x1b[34m[User]\x1b[0m\n  {}\n", ts, truncate(prompt, 300));
                    }
                    TimelineEntry::Claude { text, session_id } => {
                        let sid = session_id.as_deref().map(|s| format!(" ({})", &s[..8.min(s.len())])).unwrap_or_default();
                        println!("[{}] \x1b[32m[Claude]\x1b[0m{}\n  {}\n", ts, sid, truncate(text, 300));
                    }
                    TimelineEntry::Note { text } => {
                        println!("[{}] [Note]\n  {}\n", ts, truncate(text, 200));
                    }
                }
            }
        }

        // ── delete ─────────────────────────────────────────────────────────
        Commands::Delete { id } => {
            let mut records = read_db(&db_file)?;
            let before = records.len();
            records.retain(|r| !r.id.starts_with(&id));
            if records.len() == before {
                anyhow::bail!("no record found matching id \"{}\"", id);
            }
            write_db(&db_file, &records)?;
            rebuild_and_save_hnsw(&db_file, &records)?;
            println!("🗑️  deleted ({} remaining)", records.len());
        }

        // ── import ─────────────────────────────────────────────────────────
        Commands::Import { path, format } => {
            let is_stdin = path.to_str() == Some("-");
            let fmt = format
                .unwrap_or_else(|| {
                    if is_stdin { "txt".to_string() }
                    else { path.extension().and_then(|e| e.to_str()).unwrap_or("txt").to_string() }
                })
                .to_lowercase();

            // Read source into string (file or stdin)
            let source = if is_stdin {
                let mut buf = String::new();
                std::io::stdin().read_to_string(&mut buf).context("failed to read stdin")?;
                buf
            } else {
                std::fs::read_to_string(&path).context("failed to read file")?
            };

            let entries: Vec<ImportEntry> = match fmt.as_str() {
                "json" => {
                    serde_json::from_str(&source).context("invalid JSON format")?
                }
                "csv" => {
                    let mut rdr = csv::Reader::from_reader(source.as_bytes());
                    let mut entries = Vec::new();
                    for result in rdr.records() {
                        let r = result?;
                        let text = r.get(0).unwrap_or("").trim().to_string();
                        if text.is_empty() { continue; }
                        let tags: Vec<String> = r.get(1).unwrap_or("")
                            .split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                        entries.push(ImportEntry { text, tags });
                    }
                    entries
                }
                _ => {
                    source.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty())
                        .map(|text| ImportEntry { text, tags: vec![] }).collect()
                }
            };

            if entries.is_empty() {
                println!("no entries to import.");
                return Ok(());
            }
            println!("📦 importing {} records...", entries.len());
            let mut records = read_db(&db_file)?;
            let hp = hnsw_path(&db_file);
            let mut index = load_or_build_hnsw(&db_file, &records)?;
            let mut added = 0usize;
            for (i, entry) in entries.iter().enumerate() {
                let vector = get_embedding(&entry.text, &api_key, &cli.embedding_url, &cli.embedding_model).await?;
                let id = uuid::Uuid::new_v4().to_string();
                index.insert(id.clone(), vector.clone());
                records.push(IntentRecord {
                    id, text: entry.text.clone(), vector, timestamp: now_secs(), tags: entry.tags.clone(),
                });
                added += 1;
                if (i + 1) % 10 == 0 || i + 1 == entries.len() {
                    print!("\r   {}/{} done...", i + 1, entries.len());
                    let _ = std::io::stdout().flush();
                }
            }
            println!();
            write_db(&db_file, &records)?;
            index.save(&hp)?;
            println!("✅ imported {} records ({} total)", added, records.len());
        }

        // ── export ─────────────────────────────────────────────────────────
        Commands::Export { output, format } => {
            let records = read_db(&db_file)?;
            if records.is_empty() {
                println!("no records found.");
                return Ok(());
            }
            let content = match format.to_lowercase().as_str() {
                "csv" => {
                    let mut out = String::from("id,text,tags,timestamp\n");
                    for rec in &records {
                        let text = rec.text.replace('"', "\"\"");
                        let tags = rec.tags.join(",").replace('"', "\"\"");
                        out.push_str(&format!("\"{}\",\"{}\",\"{}\",{}\n", rec.id, text, tags, rec.timestamp));
                    }
                    out
                }
                _ => {
                    let entries: Vec<ExportEntry> = records.iter()
                        .map(|r| ExportEntry { id: &r.id, text: &r.text, tags: &r.tags, timestamp: r.timestamp })
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
            let records = read_db(&db_file)?;
            if records.len() < 2 {
                println!("need at least 2 records.");
                return Ok(());
            }
            println!("🔎 detecting duplicates (threshold: {:.2})...", threshold);
            let index = load_or_build_hnsw(&db_file, &records)?;
            let id_to_idx: HashMap<&str, usize> =
                records.iter().enumerate().map(|(i, r)| (r.id.as_str(), i)).collect();
            let mut seen_pairs: HashSet<(usize, usize)> = HashSet::new();
            let mut dup_pairs: Vec<(usize, usize, f32)> = Vec::new();
            for rec in &records {
                let raw = index.search(&rec.vector, 5, 20);
                for &(score, neighbor_id) in &raw {
                    if neighbor_id == rec.id.as_str() || score < threshold { continue; }
                    let i = id_to_idx[rec.id.as_str()];
                    let j = id_to_idx[neighbor_id];
                    let pair = (i.min(j), i.max(j));
                    if seen_pairs.insert(pair) { dup_pairs.push((pair.0, pair.1, score)); }
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
                let to_delete: HashSet<usize> = dup_pairs.iter()
                    .map(|&(i, j, _)| if records[i].timestamp >= records[j].timestamp { i } else { j })
                    .collect();
                let remaining: Vec<IntentRecord> = records.into_iter().enumerate()
                    .filter(|(i, _)| !to_delete.contains(i)).map(|(_, r)| r).collect();
                write_db(&db_file, &remaining)?;
                rebuild_and_save_hnsw(&db_file, &remaining)?;
                println!("🗑️  deleted {} record(s) ({} remaining)", to_delete.len(), remaining.len());
            } else {
                println!("run with --delete to remove duplicates automatically.");
            }
        }

        // ── summarize ──────────────────────────────────────────────────────
        Commands::Summarize { topic, tags, before, after, top } => {
            let records = read_db(&db_file)?;
            if records.is_empty() {
                println!("no records found. add some with `idb put \"your text\"`");
                return Ok(());
            }
            let before_ts = before.as_deref().map(parse_date).transpose()?;
            let after_ts = after.as_deref().map(parse_date).transpose()?;
            let filtered: Vec<&IntentRecord> = records.iter()
                .filter(|r| matches_tags(r, &tags))
                .filter(|r| before_ts.is_none_or(|b| r.timestamp < b))
                .filter(|r| after_ts.is_none_or(|a| r.timestamp >= a))
                .take(top)
                .collect();
            if filtered.is_empty() {
                println!("no records match the given filters.");
                return Ok(());
            }
            let context = filtered.iter().enumerate()
                .map(|(i, r)| format!("[{}] {}", i + 1, r.text))
                .collect::<Vec<_>>()
                .join("\n");
            let topic_line = topic.as_deref().unwrap_or("the stored records");
            println!("📝 summarizing {} record(s) about \"{}\"...\n", filtered.len(), topic_line);
            let prompt = format!(
                "Summarize the following records about {}. \
                 Identify key themes, patterns, and notable items. \
                 Be concise but comprehensive.",
                topic_line
            );
            let summary = ask_llm(&prompt, &context, &cli.llm_url, &cli.llm_model, &api_key).await?;
            println!("{}\n", summary);
            println!("{}", "─".repeat(50));
            println!("({} records used as context)", filtered.len());
        }

        // ── cluster ────────────────────────────────────────────────────────
        Commands::Cluster { k, tags } => {
            let records = read_db(&db_file)?;
            let filtered: Vec<&IntentRecord> = records.iter()
                .filter(|r| matches_tags(r, &tags))
                .collect();
            if filtered.len() < 2 {
                println!("need at least 2 records to cluster.");
                return Ok(());
            }
            let k_actual = k.min(filtered.len());
            println!("🔬 clustering {} records into {} groups...\n", filtered.len(), k_actual);
            let vecs: Vec<Vec<f32>> = filtered.iter().map(|r| r.vector.clone()).collect();
            let assignments = kmeans(&vecs, k_actual);
            // Group records by cluster
            let mut groups: Vec<Vec<&IntentRecord>> = vec![Vec::new(); k_actual];
            for (i, &c) in assignments.iter().enumerate() {
                groups[c].push(filtered[i]);
            }
            for (c, group) in groups.iter().enumerate() {
                if group.is_empty() { continue; }
                println!("── Group {} ({} records) ──", c + 1, group.len());
                for rec in group {
                    println!("  [{}...]{} {}", &rec.id[..8], format_tags(&rec.tags), rec.text);
                }
                println!();
            }
        }

        // ── serve ──────────────────────────────────────────────────────────
        Commands::Serve { port, host } => {
            let records = read_db(&db_file)?;
            let hp = hnsw_path(&db_file);
            let index = load_or_build_hnsw(&db_file, &records)?;
            let state = Arc::new(AppState {
                db_path: db_file.clone(),
                hnsw_path: hp,
                api_key,
                embedding_url: cli.embedding_url.clone(),
                embedding_model: cli.embedding_model.clone(),
                llm_url: cli.llm_url.clone(),
                llm_model: cli.llm_model.clone(),
                db: Mutex::new(DbState { records, index }),
            });
            let app = Router::new()
                .route("/records", post(handle_put))
                .route("/records", get(handle_list))
                .route("/records/:id", axum::routing::patch(handle_update))
                .route("/records/:id", delete(handle_delete))
                .route("/records/:id/related", get(handle_related))
                .route("/search", get(handle_search))
                .route("/dedup", get(handle_dedup))
                .route("/ask", post(handle_ask))
                .route("/summarize", get(handle_summarize))
                .route("/timeline", get(handle_timeline))
                .route("/export", get(handle_export))
                .route("/import", post(handle_import))
                .route("/", get(handle_ui))
                .route("/favicon.ico", get(handle_favicon))
                .with_state(state);
            let addr = format!("{}:{}", host, port);
            let listener = tokio::net::TcpListener::bind(&addr).await?;
            println!("🚀 IntentDB API server started");
            println!("   Web UI:  http://{}:{}/", host, port);
            println!("   db file: {}", db_file.display());
            println!("   embedding: {} ({})", cli.embedding_model, cli.embedding_url);
            println!("   llm: {} ({})", cli.llm_model, cli.llm_url);
            println!();
            println!("endpoints:");
            println!("  POST   /records              add a record");
            println!("  GET    /records              list records (?tag=xxx)");
            println!("  PATCH  /records/:id          update a record");
            println!("  DELETE /records/:id          delete a record");
            println!("  GET    /records/:id/related  related records (?top=5)");
            println!("  GET    /search               search (?q=xxx&top=5&tag=xxx&before=unix&after=unix&alpha=0.7&min_score=0.7)");
            println!("  GET    /dedup                detect duplicates (?threshold=0.95)");
            println!("  POST   /ask                  ask a question (RAG)");
            println!("  GET    /summarize            summarize records (?topic=xxx&tag=xxx&before=unix&after=unix&top=20)");
            axum::serve(listener, app).await?;
        }
        // ── mcp ────────────────────────────────────────────────────────────
        Commands::Mcp => {
            // All logs go to stderr to keep stdout clean for JSON-RPC frames
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
                )
                .with_writer(std::io::stderr)
                .with_ansi(false)
                .init();
            let handler = mcp::IntentDbMcpHandler::new(
                db_file,
                api_key,
                cli.embedding_url,
                cli.embedding_model,
                cli.llm_url,
                cli.llm_model,
            );
            handler.serve_stdio().await?;
        }

        // ── sync ───────────────────────────────────────────────────────────
        Commands::Sync { action } => match action {
            SyncAction::Pull { from } => {
                let from = from.trim_end_matches('/');
                println!("⬇️  pulling from {}...", from);
                let remote: Vec<IntentRecord> = reqwest::get(format!("{}/export", from))
                    .await
                    .context("failed to connect to remote")?
                    .json()
                    .await
                    .context("failed to parse remote response")?;
                let mut local = read_db(&db_file)?;
                let existing: HashSet<&str> = local.iter().map(|r| r.id.as_str()).collect();
                let new_records: Vec<IntentRecord> = remote
                    .into_iter()
                    .filter(|r| !existing.contains(r.id.as_str()))
                    .collect();
                let added = new_records.len();
                if added == 0 {
                    println!("✅ already up to date ({} records)", local.len());
                    return Ok(());
                }
                local.extend(new_records);
                write_db(&db_file, &local)?;
                rebuild_and_save_hnsw(&db_file, &local)?;
                println!("✅ pulled {} new records ({} total)", added, local.len());
            }
            SyncAction::Push { to } => {
                let to = to.trim_end_matches('/');
                println!("⬆️  pushing to {}...", to);
                let local = read_db(&db_file)?;
                let client = reqwest::Client::new();
                #[derive(Serialize)]
                struct PushBody<'a> { records: &'a Vec<IntentRecord> }
                let res: serde_json::Value = client
                    .post(format!("{}/import", to))
                    .json(&PushBody { records: &local })
                    .send()
                    .await
                    .context("failed to connect to remote")?
                    .json()
                    .await
                    .context("failed to parse remote response")?;
                let added = res["added"].as_u64().unwrap_or(0);
                let total = res["total"].as_u64().unwrap_or(0);
                println!("✅ pushed {} new records ({} total on remote)", added, total);
            }
        },
    }

    Ok(())
}
