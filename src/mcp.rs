//! MCP (Model Context Protocol) server for intentdb.
//!
//! Run with: idb mcp
//! Register in Claude Code: ~/.claude/settings.json
use crate::{
    ask_llm, get_embedding, hnsw_path, keyword_score, load_or_build_hnsw,
    matches_tags, now_secs, read_db, tokenize, write_db, IntentRecord,
};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ─── Input types ──────────────────────────────────────────────────────────────

/// Arguments for the put tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct PutArgs {
    /// Text to store (any natural language content — notes, prompts, decisions, etc.)
    pub text: String,
    /// Optional tags for filtering later (e.g. ["prompt", "work", "urgent"])
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Arguments for the search tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SearchArgs {
    /// Natural language search query
    pub query: String,
    /// Number of results to return (default: 5)
    #[serde(default = "default_top")]
    pub top: usize,
    /// Filter by tags (only records that have ALL listed tags are returned)
    #[serde(default)]
    pub tags: Vec<String>,
    /// Hybrid blend weight: 1.0 = pure semantic, 0.0 = pure keyword (default: 1.0)
    #[serde(default = "default_alpha")]
    pub alpha: f32,
}

/// Arguments for the ask tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct AskArgs {
    /// Question to answer using stored records as context
    pub question: String,
    /// Number of context records to retrieve (default: 5)
    #[serde(default = "default_top")]
    pub top: usize,
}

/// Arguments for the list tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ListArgs {
    /// Filter by tags (empty = all records)
    #[serde(default)]
    pub tags: Vec<String>,
    /// Maximum number of records to return (default: 50)
    #[serde(default = "default_limit")]
    pub limit: usize,
}


/// Arguments for the delete tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct DeleteArgs {
    /// ID of the record to delete
    pub id: String,
}

/// Arguments for the log_conversation tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct LogConversationArgs {
    /// The user's message
    pub user_text: String,
    /// Claude's response
    pub assistant_text: String,
    /// Session ID to group multiple turns (pass the same value across turns).
    /// Leave empty or omit to start a new session.
    #[serde(default)]
    pub session_id: String,
}

fn default_top() -> usize { 5 }
fn default_alpha() -> f32 { 1.0 }
fn default_limit() -> usize { 50 }

// ─── Handler ──────────────────────────────────────────────────────────────────

/// intentdb MCP server handler.
#[derive(Clone)]
pub struct IntentDbMcpHandler {
    db_file: PathBuf,
    api_key: String,
    embedding_url: String,
    embedding_model: String,
    llm_url: String,
    llm_model: String,
    tool_router: ToolRouter<Self>,
}

impl IntentDbMcpHandler {
    pub fn new(
        db_file: PathBuf,
        api_key: String,
        embedding_url: String,
        embedding_model: String,
        llm_url: String,
        llm_model: String,
    ) -> Self {
        Self {
            db_file,
            api_key,
            embedding_url,
            embedding_model,
            llm_url,
            llm_model,
            tool_router: Self::tool_router(),
        }
    }
}

// ─── Tools ────────────────────────────────────────────────────────────────────

#[tool_router]
impl IntentDbMcpHandler {
    /// Store any text in intentdb. Use this to save prompts, instructions, notes,
    /// decisions, or any information the user wants to remember for later retrieval.
    #[tool(name = "put")]
    async fn put(&self, Parameters(args): Parameters<PutArgs>) -> Result<String, String> {
        let vector = get_embedding(
            &args.text,
            &self.api_key,
            &self.embedding_url,
            &self.embedding_model,
        )
        .await
        .map_err(|e| e.to_string())?;

        let id = uuid::Uuid::new_v4().to_string();
        let mut records = read_db(&self.db_file).map_err(|e| e.to_string())?;

        records.push(IntentRecord {
            id: id.clone(),
            text: args.text.clone(),
            vector: vector.clone(),
            timestamp: now_secs(),
            tags: args.tags.clone(),
        });

        write_db(&self.db_file, &records).map_err(|e| e.to_string())?;

        // Incremental HNSW insert (fast — avoids full rebuild)
        let hp = hnsw_path(&self.db_file);
        let mut index = crate::hnsw::Hnsw::load(&hp).unwrap_or_else(|_| crate::hnsw::Hnsw::new());
        if index.len() != records.len() - 1 {
            index = crate::hnsw::Hnsw::build(
                records[..records.len() - 1]
                    .iter()
                    .map(|r| (r.id.clone(), r.vector.clone())),
            );
        }
        index.insert(id.clone(), vector);
        index.save(&hp).map_err(|e| e.to_string())?;

        tracing::info!("put: saved id={} tags={:?}", &id[..8], args.tags);
        Ok(serde_json::json!({
            "id": id,
            "text": args.text,
            "tags": args.tags,
            "total": records.len(),
        })
        .to_string())
    }

    /// Semantic search over stored records. Returns the most relevant records
    /// for a natural language query, ranked by similarity score.
    #[tool(name = "search")]
    async fn search(&self, Parameters(args): Parameters<SearchArgs>) -> Result<String, String> {
        let records = read_db(&self.db_file).map_err(|e| e.to_string())?;
        if records.is_empty() {
            return Ok("[]".to_string());
        }

        let query_vec = get_embedding(
            &args.query,
            &self.api_key,
            &self.embedding_url,
            &self.embedding_model,
        )
        .await
        .map_err(|e| e.to_string())?;

        let index =
            load_or_build_hnsw(&self.db_file, &records).map_err(|e| e.to_string())?;

        let record_map: std::collections::HashMap<&str, &IntentRecord> =
            records.iter().map(|r| (r.id.as_str(), r)).collect();
        let query_words = tokenize(&args.query);
        let top = args.top.clamp(1, 100);
        let raw = index.search(&query_vec, top * 4, 50);

        let mut scored: Vec<(f32, &IntentRecord)> = raw
            .iter()
            .filter_map(|&(sem, id)| record_map.get(id).map(|rec| (sem, *rec)))
            .filter(|(_, rec)| matches_tags(rec, &args.tags))
            .map(|(sem, rec)| {
                let score = if args.alpha >= 1.0 {
                    sem
                } else {
                    let kw = keyword_score(&rec.text, &query_words);
                    args.alpha * sem + (1.0 - args.alpha) * kw
                };
                (score, rec)
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        let results: Vec<serde_json::Value> = scored
            .iter()
            .take(top)
            .map(|(score, rec)| {
                serde_json::json!({
                    "score": score,
                    "id": rec.id,
                    "text": rec.text,
                    "tags": rec.tags,
                    "timestamp": rec.timestamp,
                })
            })
            .collect();

        serde_json::to_string_pretty(&results).map_err(|e| e.to_string())
    }

    /// Answer a question using stored records as context (RAG).
    /// Retrieves the most relevant records, then asks an LLM to synthesize an answer.
    #[tool(name = "ask")]
    async fn ask(&self, Parameters(args): Parameters<AskArgs>) -> Result<String, String> {
        let records = read_db(&self.db_file).map_err(|e| e.to_string())?;
        if records.is_empty() {
            return Err("No records found. Add some with the `put` tool first.".to_string());
        }

        let query_vec = get_embedding(
            &args.question,
            &self.api_key,
            &self.embedding_url,
            &self.embedding_model,
        )
        .await
        .map_err(|e| e.to_string())?;

        let index =
            load_or_build_hnsw(&self.db_file, &records).map_err(|e| e.to_string())?;

        let record_map: std::collections::HashMap<&str, &IntentRecord> =
            records.iter().map(|r| (r.id.as_str(), r)).collect();
        let top = args.top.clamp(1, 20);
        let raw = index.search(&query_vec, top * 2, 50);
        let sources: Vec<&IntentRecord> = raw
            .iter()
            .filter_map(|&(_, id)| record_map.get(id).copied())
            .take(top)
            .collect();

        let context = sources
            .iter()
            .enumerate()
            .map(|(i, r)| format!("[{}] {}", i + 1, r.text))
            .collect::<Vec<_>>()
            .join("\n");

        let answer = ask_llm(
            &args.question,
            &context,
            &self.llm_url,
            &self.llm_model,
            &self.api_key,
        )
        .await
        .map_err(|e| e.to_string())?;

        let source_list: Vec<serde_json::Value> = sources
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id,
                    "text": r.text,
                    "tags": r.tags,
                })
            })
            .collect();

        serde_json::to_string_pretty(&serde_json::json!({
            "answer": answer,
            "sources": source_list,
        }))
        .map_err(|e| e.to_string())
    }

    /// List stored records. Returns all records (or filtered by tag), newest first, up to the limit.
    /// Does not require embedding — instant response.
    #[tool(name = "list")]
    async fn list(&self, Parameters(args): Parameters<ListArgs>) -> Result<String, String> {
        let records = read_db(&self.db_file).map_err(|e| e.to_string())?;

        let results: Vec<serde_json::Value> = records
            .iter()
            .filter(|r| matches_tags(r, &args.tags))
            .rev()
            .take(args.limit)
            .map(|r| {
                serde_json::json!({
                    "id": r.id,
                    "text": r.text,
                    "tags": r.tags,
                    "timestamp": r.timestamp,
                })
            })
            .collect();

        serde_json::to_string_pretty(&results).map_err(|e| e.to_string())
    }

    /// Delete a record by ID. Use `list` or `search` to find the ID first.
    #[tool(name = "delete")]
    async fn delete(&self, Parameters(args): Parameters<DeleteArgs>) -> Result<String, String> {
        let client = reqwest::Client::new();
        let resp = client
            .delete(format!("http://localhost:3000/records/{}", args.id))
            .send()
            .await
            .map_err(|e| format!("DELETE failed: {e}"))?;
        if resp.status().is_success() {
            Ok(format!("deleted {}", args.id))
        } else {
            Err(format!("DELETE error: {}", resp.status()))
        }
    }

    /// Save a conversation turn (user message + assistant response) so it appears
    /// in `idb list` and `idb timeline`. Call this after each Claude response to
    /// capture conversations from Claude Desktop or other interfaces.
    #[tool(name = "log_conversation")]
    async fn log_conversation(
        &self,
        Parameters(args): Parameters<LogConversationArgs>,
    ) -> Result<String, String> {
        let session_id = if args.session_id.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            args.session_id.clone()
        };

        let user_text = serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "prompt": args.user_text,
            "session_id": session_id,
        })
        .to_string();

        let assistant_text = serde_json::json!({
            "hook_event_name": "Stop",
            "response": args.assistant_text,
            "session_id": session_id,
        })
        .to_string();

        // POST to HTTP server to avoid file write conflicts with idb serve
        let client = reqwest::Client::new();
        let base = "http://localhost:3000";

        let user_resp = client.post(format!("{}/records", base))
            .json(&serde_json::json!({"text": user_text, "tags": ["prompt", "claude-desktop"]}))
            .send().await.map_err(|e| format!("POST /records failed: {e}"))?;
        if !user_resp.status().is_success() {
            return Err(format!("POST /records error: {}", user_resp.status()));
        }

        let asst_resp = client.post(format!("{}/records", base))
            .json(&serde_json::json!({"text": assistant_text, "tags": ["response", "claude-desktop"]}))
            .send().await.map_err(|e| format!("POST /records failed: {e}"))?;
        if !asst_resp.status().is_success() {
            return Err(format!("POST /records error: {}", asst_resp.status()));
        }

        tracing::info!("log_conversation: session={}", &session_id[..8]);
        Ok(serde_json::json!({
            "session_id": session_id,
            "source": "claude-desktop",
        }).to_string())
    }

}

// ─── ServerHandler impl ───────────────────────────────────────────────────────

#[tool_handler]
impl ServerHandler for IntentDbMcpHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "intentdb MCP server. \
                 Use `put` to store prompts, instructions, notes, or any text. \
                 Use `search` to find semantically related records. \
                 Use `ask` to answer questions from stored records (RAG). \
                 Use `list` to enumerate records. \
                 Use `log_conversation` to save a user+assistant conversation turn so it \
                 appears in the timeline (call this after each response in Claude Desktop).",
            )
    }
}

// ─── Entry point ─────────────────────────────────────────────────────────────

impl IntentDbMcpHandler {
    pub async fn serve_stdio(self) -> anyhow::Result<()> {
        let service = self
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|e| anyhow::anyhow!("MCP server error: {}", e))?;
        service.waiting().await?;
        Ok(())
    }
}
