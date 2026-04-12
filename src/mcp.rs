//! MCP (Model Context Protocol) server for intentdb.
//!
//! Run with: idb mcp
//! Register in Claude Code: ~/.claude/settings.json
use crate::{
    ask_llm, classify_record, get_embedding, hnsw_path, keyword_score, load_or_build_hnsw,
    matches_tags, now_secs, read_db, tokenize, write_db, IntentRecord, TimelineEntry,
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

/// Arguments for the summarize tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SummarizeArgs {
    /// Topic to focus the summary on (optional, e.g. "billing issues", "deployment incidents")
    pub topic: Option<String>,
    /// Filter by tags
    #[serde(default)]
    pub tags: Vec<String>,
    /// Maximum number of records to use as context (default: 20)
    #[serde(default = "default_summarize_top")]
    pub top: usize,
}

/// Arguments for the timeline tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct TimelineArgs {
    /// Filter by session ID prefix (optional)
    pub session: Option<String>,
    /// Maximum entries to return (default: 50)
    #[serde(default = "default_limit")]
    pub limit: usize,
}

/// Arguments for the log_conversation tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct LogConversationArgs {
    /// The user's message
    pub user_text: String,
    /// Claude's response
    pub assistant_text: String,
    /// Session ID to group turns (auto-generated UUID if omitted)
    pub session_id: Option<String>,
    /// Source tag to identify origin, e.g. "claude-desktop", "claude-web" (default: "claude-desktop")
    pub source: Option<String>,
}

fn default_top() -> usize { 5 }
fn default_alpha() -> f32 { 1.0 }
fn default_limit() -> usize { 50 }
fn default_summarize_top() -> usize { 20 }

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

    /// Show prompts and Claude responses interleaved chronologically.
    /// Returns records sorted by timestamp ascending. Prompts show the extracted
    /// `prompt` field; responses show raw text.
    #[tool(name = "timeline")]
    async fn timeline(&self, Parameters(args): Parameters<TimelineArgs>) -> Result<String, String> {
        let records = read_db(&self.db_file).map_err(|e| e.to_string())?;
        let mut sorted: Vec<&IntentRecord> = records.iter().collect();
        sorted.sort_by_key(|r| r.timestamp);

        let results: Vec<serde_json::Value> = sorted
            .into_iter()
            .filter_map(|rec| {
                let entry = classify_record(rec);
                match &entry {
                    TimelineEntry::User { prompt, session_id } => {
                        if let Some(ref sid) = args.session {
                            if !session_id.as_deref().map(|s| s.starts_with(sid.as_str())).unwrap_or(false) {
                                return None;
                            }
                        }

                        Some(serde_json::json!({
                            "role": "user",
                            "timestamp": rec.timestamp,
                            "session_id": session_id,
                            "text": prompt,
                            "id": &rec.id[..8],
                        }))
                    }
                    TimelineEntry::Claude { text, session_id } => {
                        if let Some(ref sid) = args.session {
                            if !session_id.as_deref().map(|s| s.starts_with(sid.as_str())).unwrap_or(true) {
                                return None;
                            }
                        }
                        Some(serde_json::json!({
                            "role": "claude",
                            "timestamp": rec.timestamp,
                            "session_id": session_id,
                            "text": text,
                            "id": &rec.id[..8],
                        }))
                    }
                    TimelineEntry::Note { .. } => None,
                }
            })
            .take(args.limit)
            .collect();

        serde_json::to_string_pretty(&results).map_err(|e| e.to_string())
    }

    /// Save a conversation turn (user message + assistant response) so it appears
    /// in `idb list` and `idb timeline`. Call this after each Claude response to
    /// capture conversations from Claude Desktop or other interfaces.
    #[tool(name = "log_conversation")]
    async fn log_conversation(
        &self,
        Parameters(args): Parameters<LogConversationArgs>,
    ) -> Result<String, String> {
        let session_id = args
            .session_id
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let source = args.source.as_deref().unwrap_or("claude-desktop");

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

        let mut records = read_db(&self.db_file).map_err(|e| e.to_string())?;
        let hp = hnsw_path(&self.db_file);
        let mut index = crate::hnsw::Hnsw::load(&hp).unwrap_or_else(|_| crate::hnsw::Hnsw::new());

        // Rebuild index if out of sync
        if index.len() != records.len() {
            index = crate::hnsw::Hnsw::build(
                records.iter().map(|r| (r.id.clone(), r.vector.clone())),
            );
        }

        // Save user turn
        let user_vec = get_embedding(&user_text, &self.api_key, &self.embedding_url, &self.embedding_model)
            .await.map_err(|e| e.to_string())?;
        let user_id = uuid::Uuid::new_v4().to_string();
        index.insert(user_id.clone(), user_vec.clone());
        records.push(IntentRecord {
            id: user_id.clone(),
            text: user_text,
            vector: user_vec,
            timestamp: now_secs(),
            tags: vec!["prompt".to_string(), source.to_string()],
        });

        // Save assistant turn
        let asst_vec = get_embedding(&assistant_text, &self.api_key, &self.embedding_url, &self.embedding_model)
            .await.map_err(|e| e.to_string())?;
        let asst_id = uuid::Uuid::new_v4().to_string();
        index.insert(asst_id.clone(), asst_vec.clone());
        records.push(IntentRecord {
            id: asst_id.clone(),
            text: assistant_text,
            vector: asst_vec,
            timestamp: now_secs(),
            tags: vec!["response".to_string(), source.to_string()],
        });

        write_db(&self.db_file, &records).map_err(|e| e.to_string())?;
        index.save(&hp).map_err(|e| e.to_string())?;

        tracing::info!("log_conversation: session={} source={}", &session_id[..8], source);
        Ok(serde_json::json!({
            "session_id": session_id,
            "source": source,
            "user_id": user_id,
            "assistant_id": asst_id,
            "total": records.len(),
        }).to_string())
    }

    /// Summarize stored records using an LLM. Useful for generating digests,
    /// weekly summaries, or understanding what's in a tag category.
    #[tool(name = "summarize")]
    async fn summarize(
        &self,
        Parameters(args): Parameters<SummarizeArgs>,
    ) -> Result<String, String> {
        let records = read_db(&self.db_file).map_err(|e| e.to_string())?;

        let filtered: Vec<&IntentRecord> = records
            .iter()
            .filter(|r| matches_tags(r, &args.tags))
            .take(args.top)
            .collect();

        if filtered.is_empty() {
            return Ok("No records found matching the given filters.".to_string());
        }

        let context = filtered
            .iter()
            .enumerate()
            .map(|(i, r)| format!("[{}] {}", i + 1, r.text))
            .collect::<Vec<_>>()
            .join("\n");

        let topic_line = args.topic.as_deref().unwrap_or("the stored records");
        let prompt = format!(
            "Summarize the following records about {}. \
             Identify key themes, patterns, and notable items. \
             Be concise but comprehensive.",
            topic_line
        );

        let summary = ask_llm(
            &prompt,
            &context,
            &self.llm_url,
            &self.llm_model,
            &self.api_key,
        )
        .await
        .map_err(|e| e.to_string())?;

        serde_json::to_string_pretty(&serde_json::json!({
            "summary": summary,
            "record_count": filtered.len(),
        }))
        .map_err(|e| e.to_string())
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
                 Use `summarize` to get an LLM summary of stored records. \
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
