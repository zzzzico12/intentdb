# intentdb

> A schema-free, intent-native storage engine. Put data in plain language. Search in plain language.

[![CI](https://img.shields.io/github/actions/workflow/status/zzzzico12/intentdb/ci.yml?style=flat-square)](https://github.com/zzzzico12/intentdb/actions)
[![crates.io](https://img.shields.io/crates/v/intentdb?style=flat-square)](https://crates.io/crates/intentdb)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](LICENSE)

```bash
# Put anything. No schema, no columns, no types.
$ idb put "Tanaka bought Product A in March 2024"
$ idb put "Suzuki contacted support last week about a billing issue"
$ idb put "Yamada has been a loyal customer for 3 years"

# Search in plain language.
$ idb search "customers who had problems recently"

1. [score: 0.941] Suzuki contacted support last week about a billing issue
2. [score: 0.812] Tanaka bought Product A in March 2024
```

---

## Why intentdb?

Every database requires you to design a schema before you store anything.  
intentdb does not. Just put text in. Ask questions later.

| | Traditional DB | Vector DB | **intentdb** |
|---|---|---|---|
| Schema required | ✅ Yes | ⚠️ Partial | ❌ None |
| Natural language query | ❌ | ⚠️ With glue code | ✅ Native |
| Storage engine | Off-the-shelf | Off-the-shelf | **Custom (.idb)** |
| Index type | B-tree | HNSW (library) | **HNSW (from scratch)** |
| Single binary | ❌ | ❌ | ✅ |

intentdb is built on a custom binary file format (`.idb`) with an HNSW graph index written from scratch in Rust — not a wrapper around PostgreSQL, SQLite, or Faiss.

---

## Install

```bash
cargo install intentdb
```

Or build from source:

```bash
git clone https://github.com/zzzzico12/intentdb
cd intentdb
cargo build --release
# Add to PATH, or use ./target/release/idb directly
```

Set your OpenAI API key:

```bash
export OPENAI_API_KEY=sk-...
```

**Requirements:** Rust 1.75+, OpenAI API key (or Ollama for local inference)

---

## Quickstart (30 seconds)

```bash
# Add records — anything goes, no schema needed
idb put "Alice closed a $50k deal on Friday"
idb put "Bob's server went down at 2am, resolved by morning"
idb put "Carol has been asking about the enterprise plan"

# Add records with tags
idb put "Dave reported a login bug" --tag bug --tag urgent

# Search with natural language
idb search "recent incidents"
idb search "sales opportunities"
idb search "customers interested in upgrading"

# Filter by tag
idb search "bugs" --tag urgent

# Only show high-confidence results
idb search "billing issue" --min-score 0.75

# Time-travel: filter by date
idb search "recent incidents" --after 2024-01-01
idb search "old issues" --before 2024-06-01 --after 2024-01-01

# Hybrid search (semantic + keyword blend)
idb search "login bug" --alpha 0.7   # 70% semantic, 30% keyword

# Ask a question — RAG (retrieves relevant context, then answers via LLM)
idb ask "What customer issues happened last week?"

# Summarize stored records via LLM
idb summarize                                      # all records
idb summarize "billing issues" --tag support       # focused topic
idb summarize --after 2024-06-01                   # time-bounded

# Cluster records by semantic similarity
idb cluster --k 5
idb cluster --k 3 --tag support

# Namespaces (isolated data sets in the same directory)
idb --ns sales put "Alice closed a deal"
idb --ns incidents put "Bob's server went down"
idb --ns sales search "recent deals"

# List all records
idb list
idb list --tag bug

# Update a record (re-embeds automatically)
idb update <id> "Updated text here"

# Delete a record
idb delete <id>

# Find semantically related records by ID
idb related <id> --top 5

# Detect duplicates
idb dedup --threshold 0.95
idb dedup --threshold 0.95 --delete

# Import from files
idb import data.json      # [{"text": "...", "tags": ["a", "b"]}, ...]
idb import data.csv       # text column, optional tags column (comma-separated)
idb import notes.txt      # one record per line

# Import from stdin (pipe-friendly)
cat errors.txt | idb import -
echo "quick note" | idb import -
tail -f app.log | idb import - --format txt

# Export (no vectors)
idb export --format json -o backup.json
idb export --format csv -o backup.csv
```

---

## Ollama (local, no API key needed)

Run intentdb fully offline using [Ollama](https://ollama.com):

```bash
# Pull models
ollama pull nomic-embed-text
ollama pull llama3

# Point intentdb at Ollama's OpenAI-compatible endpoints
export IDB_EMBEDDING_URL=http://localhost:11434/v1/embeddings
export IDB_EMBEDDING_MODEL=nomic-embed-text
export IDB_LLM_URL=http://localhost:11434/v1/chat/completions
export IDB_LLM_MODEL=llama3

# No OPENAI_API_KEY needed
idb put "Alice closed a deal"
idb search "recent sales"
idb ask "Who closed deals recently?"
idb summarize "this week's activity"
```

Or pass flags directly:

```bash
idb --embedding-url http://localhost:11434/v1/embeddings \
    --embedding-model nomic-embed-text \
    search "recent sales"
```

| Env var | CLI flag | Default |
|---|---|---|
| `OPENAI_API_KEY` | — | *(empty — not needed for Ollama)* |
| `IDB_EMBEDDING_URL` | `--embedding-url` | `https://api.openai.com/v1/embeddings` |
| `IDB_EMBEDDING_MODEL` | `--embedding-model` | `text-embedding-3-small` |
| `IDB_LLM_URL` | `--llm-url` | `https://api.openai.com/v1/chat/completions` |
| `IDB_LLM_MODEL` | `--llm-model` | `gpt-4o-mini` |

---

## Web UI

When you run `idb serve`, a browser UI is automatically available at `http://localhost:3000/`.

No separate install needed — the UI is embedded in the binary.

**Tabs:**
- **Search** — semantic search with all filters (tag, date range, α, min-score)
- **Ask** — RAG question answering with cited sources
- **Add** — add records with tags (Cmd+Enter to save)
- **Browse** — list all records; tag cloud at the top lets you filter by clicking a tag; delete individual records
- **Summarize** — LLM summary with topic / tag / date filters
- **Timeline** — conversation history view: User prompts and Claude responses interleaved chronologically
- **Import** — paste or upload JSON / CSV / TXT directly from the browser

---

## Multi-device sync

intentdb servers can sync records with each other. Each sync operation is **additive** — it only adds records that the target is missing, identified by record ID. No data is overwritten or deleted.

```bash
# Pull records from a remote server into local DB
idb sync pull --from http://192.168.1.10:3000

# Push local records to a remote server
idb sync push --to http://192.168.1.10:3000

# Two-way sync: pull first, then push
idb sync pull --from http://192.168.1.10:3000
idb sync push --to http://192.168.1.10:3000
```

**Typical setup:** Run `idb serve` on a shared machine (home server, VPS, etc.) and sync from each device.

```bash
# Shared server (always running)
idb serve --host 0.0.0.0 --port 3000 --file /data/shared.idb

# Device A — sync after adding records
idb put "Meeting notes from today"
idb sync push --to http://myserver:3000

# Device B — pull before searching
idb sync pull --from http://myserver:3000
idb search "meeting notes"
```

---

## HTTP API

intentdb also runs as a local HTTP server:

```bash
idb serve --port 3000
```

```bash
# Add a record
curl -X POST http://localhost:3000/records \
  -H "Content-Type: application/json" \
  -d '{"text": "Alice closed a deal", "tags": ["sales"]}'

# Search (time-travel, hybrid blend, min-score)
curl "http://localhost:3000/search?q=recent+sales&top=5"
curl "http://localhost:3000/search?q=bugs&top=5&tag=urgent"
curl "http://localhost:3000/search?q=incidents&after=1704067200&before=1717200000"
curl "http://localhost:3000/search?q=login+bug&alpha=0.7&min_score=0.6"

# Ask (RAG)
curl -X POST http://localhost:3000/ask \
  -H "Content-Type: application/json" \
  -d '{"question": "What customer issues happened last week?", "top": 5}'

# Summarize
curl "http://localhost:3000/summarize"
curl "http://localhost:3000/summarize?topic=billing+issues&tag=support&top=20"

# List
curl "http://localhost:3000/records"
curl "http://localhost:3000/records?tag=sales"

# Update
curl -X PATCH http://localhost:3000/records/<id> \
  -H "Content-Type: application/json" \
  -d '{"text": "Updated text"}'

# Delete
curl -X DELETE http://localhost:3000/records/<id>

# Related records
curl "http://localhost:3000/records/<id>/related?top=5"

# Duplicate detection
curl "http://localhost:3000/dedup?threshold=0.95"

# Timeline — User prompts and Claude responses interleaved chronologically
curl "http://localhost:3000/timeline"

# Tags — list all tags with record counts, sorted by count descending
curl "http://localhost:3000/tags"

# Ingest — bulk import via HTTP (JSON array, CSV, or plain text)
curl -X POST "http://localhost:3000/ingest?format=json" \
  -H "Content-Type: text/plain" \
  -d '[{"text": "Alice closed a deal", "tags": ["sales"]}]'

curl -X POST "http://localhost:3000/ingest?format=txt&tag=log" \
  --data-binary @notes.txt
```

### Python client

```python
# No dependencies — stdlib only
from intentdb import Client

db = Client("http://localhost:3000")

db.put("Alice closed a $50k deal on Friday", tags=["sales"])
db.put("Bob's server went down at 2am", tags=["incident"])

results = db.search("recent problems", top=3)
for r in results:
    print(f"[{r['score']:.3f}] {r['text']}")

# Related records
related = db.related("<id>", top=5)

# Duplicate detection
pairs = db.dedup(threshold=0.95)
```

Copy [python/intentdb.py](python/intentdb.py) into your project — no pip install needed (stdlib only).

---

## Command reference

### `idb put <text>`

Store any piece of information as a record. The text is automatically converted to a vector embedding — no schema, no column definitions needed.

**When to use:** Any time you want to capture something for later retrieval. Treat it like a smart notebook: customer meeting notes, bug reports, ideas, log messages, observations — anything expressed in natural language.

```bash
idb put "Alice called to say she's ready to upgrade to the enterprise plan"
idb put "Fixed the memory leak in the image processor — root cause was unbounded cache" --tag fix
idb put "Team decided to deprecate the v1 API by end of Q3" --tag decision
```

---

### `idb search <query>`

Find the most semantically relevant records for a natural language query. Returns ranked results by similarity score.

**When to use:** When you want to retrieve something but don't remember the exact wording. Think of it as asking "what do I have that's related to X?" rather than keyword matching. Combine flags to narrow the result set.

```bash
idb search "customers who mentioned pricing concerns"
idb search "performance issues" --tag production --after 2024-01-01
idb search "login bug" --alpha 0.6 --min-score 0.7   # hybrid + quality gate
```

---

### `idb ask <question>`

Ask a question in plain language. intentdb retrieves the most relevant stored records, then passes them as context to an LLM to produce a direct answer (RAG pipeline).

**When to use:** When you want an answer synthesized from your stored knowledge, not just a list of matching records. Great for "give me a summary of what's happening with X" or "has anyone reported this kind of problem before?"

```bash
idb ask "What was the root cause of the outage last month?"
idb ask "Which customers are most at risk of churning?"
idb ask "What open action items do we have from recent meetings?"
```

---

### `idb summarize [topic]`

Asks an LLM to summarize a group of records, optionally focused on a topic. Filters by tag and date range let you scope the summary precisely.

**When to use:** For periodic digests — end-of-week reviews, incident post-mortems, sales pipeline summaries. Instead of reading through dozens of records manually, get a coherent paragraph of key themes and patterns.

```bash
idb summarize "customer feedback" --tag support --after 2024-06-01
idb summarize "deployment incidents" --tag incident
idb summarize                          # full summary of all records
```

---

### `idb cluster --k <N>`

Groups records into N clusters based on semantic similarity using k-means on embeddings. Records that are conceptually related end up in the same group — even if they use different words.

**When to use:** When your data has grown and you want to understand what topics or themes are present without reading everything. Useful for exploring a new dataset, reorganizing a knowledge base, or deciding which tag categories to introduce.

```bash
idb cluster --k 5               # find 5 natural groupings across all records
idb cluster --k 3 --tag support # group support tickets into 3 themes
```

---

### `idb list`

Display all stored records in insertion order. Optionally filter by tag.

**When to use:** When you want a quick overview of everything stored, or when auditing what's in a namespace or tag category. Unlike `search`, this returns everything without ranking.

```bash
idb list
idb list --tag urgent
idb --ns sales list
```

---

### `idb timeline`

Display stored User prompts and Claude responses interleaved in chronological order. Useful for reviewing conversation history captured via Claude Code hooks.

Records are automatically classified:
- **[User]** — records stored by the `UserPromptSubmit` hook (JSON with `hook_event_name: "UserPromptSubmit"`)
- **[Claude]** — records stored by the `Stop` hook (tagged `response`)
- Notes and other records are hidden by default

**When to use:** When you want to review the conversation history that was captured, debug what was stored, or trace back a specific session.

```bash
idb timeline                        # show all sessions, oldest first
idb timeline --limit 20             # show the last 20 entries
idb timeline --session abc123       # filter by session ID prefix
idb timeline --show-notes           # also show unclassified records
```

Also available as `GET /timeline` in the HTTP API and as a **Timeline** tab in the Web UI.

---

### `idb update <id> <text>`

Replace the text of an existing record. The new text is re-embedded automatically so future searches reflect the updated content.

**When to use:** When an earlier record needs to be corrected or expanded — a bug that got resolved, a deal that changed status, a decision that was revised. Use the first 8 characters of the ID shown in `list` or `search` output.

```bash
idb update a3f9c2b1 "Alice upgraded to the enterprise plan — deal closed at $80k"
idb update 7d2e4a0f "Memory leak fixed in v2.3.1 — deployed to production" --tag fix --tag resolved
```

---

### `idb delete <id>`

Remove a record permanently by ID prefix.

**When to use:** When a record is outdated, was entered by mistake, or is a duplicate you want to clean up manually. The HNSW index is rebuilt automatically after deletion.

```bash
idb delete a3f9c2b1
```

---

### `idb related <id>`

Find records that are semantically similar to a given record, ranked by cosine similarity.

**When to use:** When you find one relevant record and want to discover others like it — similar past incidents, related customer feedback, or prior art for a decision. Useful as a "more like this" feature.

```bash
idb related a3f9c2b1 --top 10
```

---

### `idb dedup`

Scan all records and surface pairs that are semantically near-identical (above a similarity threshold). Optionally auto-delete the newer duplicate.

**When to use:** After bulk imports, or after accumulating data over time, to clean up accidental duplicates. The `--threshold` controls strictness: 0.99 catches near-verbatim copies; 0.90 catches paraphrases of the same fact.

```bash
idb dedup --threshold 0.97          # review pairs
idb dedup --threshold 0.97 --delete # auto-remove newer duplicate in each pair
```

---

### `idb import <file | ->`

Bulk-load records from a JSON array, CSV file, plain text (one line = one record), or stdin. Embeddings are generated for each entry automatically.

**When to use:** When migrating existing data into intentdb — Notion exports, CSV customer lists, legacy log archives, or any text collection. Use `-` to read from stdin for live piping.

```bash
idb import customers.csv
idb import meeting_notes.json --tag meeting
cat raw_logs.txt | idb import - --tag log
```

---

### `idb export`

Export all records to JSON or CSV (vectors are excluded). Useful for backups, sharing, or feeding data into other tools.

**When to use:** Periodic backups, data migration to another system, or generating a human-readable snapshot of your stored knowledge.

```bash
idb export --format json -o backup.json
idb export --format csv -o snapshot.csv
```

---

### `idb serve`

Start a local HTTP API server. All CLI features are available as REST endpoints, enabling integration with scripts, web apps, and other services.

**When to use:** When you want to use intentdb from a web application, a Python script, a CI pipeline, or any system that can make HTTP requests. The Python client (`python/intentdb.py`) wraps this API with no extra dependencies.

```bash
idb serve --port 3000
idb serve --port 8080 --host 0.0.0.0   # expose on all interfaces
```

---

### Search options

| Flag | Default | Description |
|---|---|---|
| `--top N` | 5 | Number of results to return |
| `--tag <tag>` | — | Filter to records that have this tag (repeatable) |
| `--after YYYY-MM-DD` | — | Only records stored after this date |
| `--before YYYY-MM-DD` | — | Only records stored before this date |
| `--alpha 0.0–1.0` | 1.0 | Score blend: 1.0 = pure semantic, 0.0 = pure keyword |
| `--min-score 0.0–1.0` | 0.0 | Exclude results below this similarity score |

---

## Architecture

intentdb is a purpose-built storage engine, not a layer on top of an existing database.

```
┌─────────────────────────────────────────┐
│              CLI / HTTP API             │
├─────────────────────────────────────────┤
│         Natural Language Query Engine   │
│     (query → embedding → HNSW search)  │
├─────────────────────────────────────────┤
│         HNSW Index (from scratch)       │
│    Hierarchical Navigable Small World   │
├─────────────────────────────────────────┤
│      Custom File Format  (.idb)         │
│  [MAGIC][N records][vector + tags]...   │
└─────────────────────────────────────────┘
```

### .idb file format

```
[MAGIC: 4B "IDB2"]
[record count: u32]
[record 1]
  [id length: u16][id bytes]
  [text length: u32][text bytes]
  [vector dims: u32][f32 × N]
  [timestamp: u64]
  [tag count: u16]
    [tag length: u16][tag bytes] × N
[record 2] ...
```

HNSW graph is stored separately in a `.hnsw` file (same basename as `.idb`), rebuilt automatically if missing or out of sync.

No dependency on SQLite, PostgreSQL, RocksDB, or any existing storage engine.

---

## Benchmarks

Estimated on Apple M2, 1536-dim vectors (OpenAI `text-embedding-3-small`), M=16, ef=50.

| Records | Linear scan | intentdb (HNSW) | Speedup |
|---------|-------------|-----------------|---------|
| 1,000   | ~8ms        | ~0.4ms          | ~20×    |
| 10,000  | ~80ms       | ~1.2ms          | ~67×    |
| 100,000 | ~820ms      | ~4.8ms          | ~170×   |

---

## Use cases

- **Prompt library** — Store and retrieve AI prompts by meaning, not title
- **Personal knowledge base** — Dump notes freely, search semantically
- **Code snippets** — "find the one that reads a file line by line"
- **Customer notes** — CRM without the schema
- **Error log search** — "find past incidents similar to this one"
- **Daily logs** — Free-form entries, meaningful retrieval
- **Log pipeline** — Pipe `tail -f app.log | idb import -` to capture events live
- **Weekly digest** — `idb summarize "this week" --after 2024-01-01` for auto-reports

---

## Roadmap

- [x] Custom `.idb` file format
- [x] HNSW index (from scratch in Rust)
- [x] Natural language put / search / list / delete / update
- [x] Metadata & tag filtering
- [x] HTTP API
- [x] Bulk import (JSON, CSV, TXT, stdin pipe)
- [x] Export (JSON, CSV)
- [x] Duplicate detection
- [x] Related record discovery
- [x] `cargo install intentdb` on crates.io
- [x] Python client (`python/intentdb.py`, stdlib only)
- [x] Docker image
- [x] Ollama / local LLM support (`--embedding-url`, `--llm-url`)
- [x] `ask` command — RAG over stored records
- [x] `summarize` command — LLM summary of stored records
- [x] `cluster` command — semantic k-means grouping
- [x] Time-travel queries (`--before`, `--after`)
- [x] Hybrid search (`--alpha` semantic + keyword blend)
- [x] Minimum score filter (`--min-score`)
- [x] Namespaces (`--ns`)
- [x] Web UI (served at `http://localhost:3000/` when running `idb serve`)
- [x] Multi-device sync (`idb sync push/pull`)
- [x] `timeline` command — conversation history view (User + Claude interleaved chronologically)
- [x] MCP server (`idb mcp`) — native Claude Code integration via Model Context Protocol
- [x] `GET /tags` endpoint — tag list with record counts
- [x] Browse tag cloud — click a tag to filter instantly
- [x] Import tab in Web UI — paste or upload JSON / CSV / TXT from the browser

---

## Contributing

Issues and PRs are welcome.  
If you find intentdb useful, please consider giving it a ⭐ — it helps others discover the project.

```bash
git clone https://github.com/zzzzico12/intentdb
cd intentdb
cargo build
```

---

## License

MIT © zzzzico12

---

---

# intentdb（日本語）

> スキーマ不要・意図ネイティブなストレージエンジン。自然言語でデータを入れて、自然言語で検索する。

```bash
# 何でも入れられる。スキーマ定義もカラム設計も不要。
$ idb put "田中さんが2024年3月に製品Aを購入"
$ idb put "鈴木さんが先週、請求に関する問題でサポートに連絡してきた"
$ idb put "山田さんは3年来のロイヤルカスタマー"

# 自然言語で検索。
$ idb search "最近トラブルがあった顧客"

1. [score: 0.941] 鈴木さんが先週、請求に関する問題でサポートに連絡してきた
2. [score: 0.812] 田中さんが2024年3月に製品Aを購入
```

---

## なぜ intentdb？

従来のデータベースは、データを保存する前にスキーマを設計しなければなりません。  
intentdb は違います。テキストをそのまま入れて、後から質問するだけ。

| | 従来のDB | ベクターDB | **intentdb** |
|---|---|---|---|
| スキーマ定義 | ✅ 必須 | ⚠️ 一部必要 | ❌ 不要 |
| 自然言語クエリ | ❌ | ⚠️ 追加実装が必要 | ✅ ネイティブ対応 |
| ストレージエンジン | 既製品 | 既製品 | **独自実装 (.idb)** |
| インデックス種別 | B-tree | HNSW（ライブラリ） | **HNSW（ゼロから実装）** |
| シングルバイナリ | ❌ | ❌ | ✅ |

intentdb はカスタムバイナリファイル形式 (`.idb`) と、Rustでゼロから書いたHNSWグラフインデックスで動作しており、PostgreSQL・SQLite・Faiss などの既存エンジンのラッパーではありません。

---

## インストール

```bash
cargo install intentdb
```

ソースからビルドする場合：

```bash
git clone https://github.com/zzzzico12/intentdb
cd intentdb
cargo build --release
# PATHに追加するか、./target/release/idb を直接使う
```

OpenAI APIキーを設定：

```bash
export OPENAI_API_KEY=sk-...
```

**動作要件：** Rust 1.75以上、OpenAI APIキー（Ollamaを使えばローカルのみでも可）

---

## クイックスタート（30秒）

```bash
# レコードを追加 — スキーマ不要、何でも入れられる
idb put "アリスが金曜日に5万ドルの契約を締結"
idb put "ボブのサーバーが午前2時にダウン、朝までに復旧"
idb put "キャロルがエンタープライズプランへの移行を検討中"

# タグ付きで追加
idb put "デイブがログインのバグを報告" --tag bug --tag urgent

# 自然言語で検索
idb search "最近のインシデント"
idb search "営業機会"
idb search "アップグレードに興味のある顧客"

# タグで絞り込み
idb search "バグ" --tag urgent

# スコアが高いものだけ表示
idb search "請求の問題" --min-score 0.75

# 日付で絞り込み（タイムトラベルクエリ）
idb search "最近のインシデント" --after 2024-01-01
idb search "古い問題" --before 2024-06-01 --after 2024-01-01

# ハイブリッド検索（意味的類似度 + キーワードの組み合わせ）
idb search "ログインバグ" --alpha 0.7   # 70% 意味的、30% キーワード

# 質問に回答（RAG）
idb ask "先週どんな顧客からの問い合わせがありましたか？"

# LLMによるレコードのサマリー
idb summarize                                        # 全レコードを要約
idb summarize "請求関連の問題" --tag support         # トピックを絞って要約
idb summarize --after 2024-06-01                     # 期間を絞って要約

# 意味的類似度によるクラスタリング
idb cluster --k 5
idb cluster --k 3 --tag support

# ネームスペース（同じディレクトリに独立したデータセット）
idb --ns sales put "アリスが契約を締結"
idb --ns incidents put "ボブのサーバーがダウン"
idb --ns sales search "最近の商談"

# 全レコード一覧
idb list
idb list --tag bug

# レコードを更新（自動的に再エンベッド）
idb update <id> "更新後のテキスト"

# レコードを削除
idb delete <id>

# 意味的に関連するレコードを探す
idb related <id> --top 5

# 重複を検出・削除
idb dedup --threshold 0.95
idb dedup --threshold 0.95 --delete

# ファイルから一括インポート
idb import data.json      # [{"text": "...", "tags": ["a", "b"]}, ...]
idb import data.csv       # text列、任意でtags列（カンマ区切り）
idb import notes.txt      # 1行1レコード

# 標準入力からインポート（パイプ対応）
cat errors.txt | idb import -
echo "メモ" | idb import -
tail -f app.log | idb import - --format txt

# エクスポート（ベクトルは除外）
idb export --format json -o backup.json
idb export --format csv -o backup.csv
```

---

## Ollama（ローカル・APIキー不要）

[Ollama](https://ollama.com) を使えばインターネット接続なしで完全ローカル動作：

```bash
# モデルをダウンロード
ollama pull nomic-embed-text
ollama pull llama3

# intentdb の向き先を Ollama に変更
export IDB_EMBEDDING_URL=http://localhost:11434/v1/embeddings
export IDB_EMBEDDING_MODEL=nomic-embed-text
export IDB_LLM_URL=http://localhost:11434/v1/chat/completions
export IDB_LLM_MODEL=llama3

# OPENAI_API_KEY は不要
idb put "アリスが契約を締結"
idb search "最近の営業活動"
idb ask "最近契約を取ったのは誰ですか？"
idb summarize "今週の活動サマリー"
```

フラグで直接指定することも可能：

```bash
idb --embedding-url http://localhost:11434/v1/embeddings \
    --embedding-model nomic-embed-text \
    search "最近の営業活動"
```

| 環境変数 | CLIフラグ | デフォルト値 |
|---|---|---|
| `OPENAI_API_KEY` | — | *（空文字列 — Ollama利用時は不要）* |
| `IDB_EMBEDDING_URL` | `--embedding-url` | `https://api.openai.com/v1/embeddings` |
| `IDB_EMBEDDING_MODEL` | `--embedding-model` | `text-embedding-3-small` |
| `IDB_LLM_URL` | `--llm-url` | `https://api.openai.com/v1/chat/completions` |
| `IDB_LLM_MODEL` | `--llm-model` | `gpt-4o-mini` |

---

## Web UI

`idb serve` を起動すると、ブラウザUIが `http://localhost:3000/` で自動的に利用できます。

UIはバイナリに埋め込まれているため、別途インストールは不要です。

**タブ一覧：**
- **Search** — タグ・日付・α・min-scoreなどすべてのフィルターに対応した意味的検索
- **Ask** — 根拠となるソース付きのRAG回答
- **Add** — タグ付きでレコードを追加（Cmd+Enterで保存）
- **Browse** — 全レコードの一覧; 上部のタグクラウドからタグをクリックして即時フィルタ; 個別削除
- **Summarize** — トピック・タグ・日付フィルター付きのLLMサマリー
- **Timeline** — ユーザープロンプトとClaudeの回答を時系列で交互表示する会話履歴ビュー
- **Import** — JSON / CSV / TXT をブラウザから直接ペーストまたはファイルアップロードして一括登録

---

## マルチデバイス同期

intentdbサーバー同士でレコードを同期できます。同期は**追加のみ**で動作し、レコードIDで重複を検出して、相手が持っていないレコードだけを追加します。データの上書きや削除は行いません。

```bash
# リモートサーバーからローカルにレコードを取り込む
idb sync pull --from http://192.168.1.10:3000

# ローカルのレコードをリモートサーバーに送る
idb sync push --to http://192.168.1.10:3000

# 双方向同期: pull してから push
idb sync pull --from http://192.168.1.10:3000
idb sync push --to http://192.168.1.10:3000
```

**典型的な使い方：** 共有マシン（自宅サーバー、VPSなど）で `idb serve` を常時起動し、各デバイスから sync。

```bash
# 共有サーバー（常時起動）
idb serve --host 0.0.0.0 --port 3000 --file /data/shared.idb

# デバイスA — レコード追加後に push
idb put "今日の会議のメモ"
idb sync push --to http://myserver:3000

# デバイスB — 検索前に pull
idb sync pull --from http://myserver:3000
idb search "会議のメモ"
```

---

## HTTP API

intentdb はローカルHTTPサーバーとしても動作します：

```bash
idb serve --port 3000
```

```bash
# レコードを追加
curl -X POST http://localhost:3000/records \
  -H "Content-Type: application/json" \
  -d '{"text": "アリスが契約を締結", "tags": ["sales"]}'

# 検索（タイムトラベル・ハイブリッド・min-score対応）
curl "http://localhost:3000/search?q=最近の営業&top=5"
curl "http://localhost:3000/search?q=バグ&top=5&tag=urgent"
curl "http://localhost:3000/search?q=インシデント&after=1704067200&before=1717200000"
curl "http://localhost:3000/search?q=ログインバグ&alpha=0.7&min_score=0.6"

# 質問（RAG）
curl -X POST http://localhost:3000/ask \
  -H "Content-Type: application/json" \
  -d '{"question": "先週どんな問い合わせがありましたか？", "top": 5}'

# サマリー
curl "http://localhost:3000/summarize"
curl "http://localhost:3000/summarize?topic=請求の問題&tag=support&top=20"

# 一覧
curl "http://localhost:3000/records"
curl "http://localhost:3000/records?tag=sales"

# 更新
curl -X PATCH http://localhost:3000/records/<id> \
  -H "Content-Type: application/json" \
  -d '{"text": "更新後のテキスト"}'

# 削除
curl -X DELETE http://localhost:3000/records/<id>

# 関連レコード
curl "http://localhost:3000/records/<id>/related?top=5"

# 重複検出
curl "http://localhost:3000/dedup?threshold=0.95"

# タイムライン — ユーザープロンプトとClaudeの回答を時系列で交互表示
curl "http://localhost:3000/timeline"

# タグ一覧 — レコード数付き、件数降順
curl "http://localhost:3000/tags"

# Ingest — HTTP経由の一括インポート（JSON配列・CSV・テキスト）
curl -X POST "http://localhost:3000/ingest?format=json" \
  -H "Content-Type: text/plain" \
  -d '[{"text": "アリスが契約を締結", "tags": ["sales"]}]'

curl -X POST "http://localhost:3000/ingest?format=txt&tag=log" \
  --data-binary @notes.txt
```

### Python クライアント

```python
# 外部依存なし — 標準ライブラリのみ
from intentdb import Client

db = Client("http://localhost:3000")

db.put("アリスが金曜日に5万ドルの契約を締結", tags=["sales"])
db.put("ボブのサーバーが午前2時にダウン", tags=["incident"])

results = db.search("最近の問題", top=3)
for r in results:
    print(f"[{r['score']:.3f}] {r['text']}")

# 関連レコード
related = db.related("<id>", top=5)

# 重複検出
pairs = db.dedup(threshold=0.95)
```

[python/intentdb.py](python/intentdb.py) をプロジェクトにコピーするだけ — pip install 不要。

---

## コマンドリファレンス

### `idb put <text>`

テキストをレコードとして保存します。テキストは自動的にベクトルエンベッドに変換されます。スキーマ定義やカラム設計は不要です。

**使いどころ：** 情報を後から検索・参照したいときはいつでも。顧客との会話メモ、バグ報告、チームの意思決定、作業ログなど、自然言語で表現できるものは何でも保存できます。

```bash
idb put "アリスからエンタープライズプランへの移行を検討したいと連絡があった"
idb put "画像処理の無制限キャッシュによるメモリリークを修正" --tag fix
idb put "v1 APIをQ3末に廃止する方針を決定" --tag decision
```

---

### `idb search <query>`

自然言語クエリに対して、意味的に関連するレコードを類似度スコア順で返します。

**使いどころ：** 「あの件なんて言ったっけ」というときの検索。キーワードが一致しなくても、意味が近ければヒットします。`--tag`・`--after`・`--min-score` などのフラグを組み合わせて精度を高められます。

```bash
idb search "価格について不満を言っていた顧客"
idb search "本番環境のパフォーマンス問題" --tag production --after 2024-01-01
idb search "ログインバグ" --alpha 0.6 --min-score 0.7
```

---

### `idb ask <question>`

自然言語で質問すると、関連レコードを自動的に検索してコンテキストとして渡し、LLMが回答を生成します（RAGパイプライン）。

**使いどころ：** レコードのリストではなく「まとめた回答」が欲しいとき。「先月の障害の原因は？」「このエラーに似た過去事例は？」のように、蓄積した知識に対して自然な質問ができます。

```bash
idb ask "先月の障害の根本原因は何でしたか？"
idb ask "解約リスクが高い顧客はどれですか？"
idb ask "直近の会議で出た未解決のアクションアイテムは？"
```

---

### `idb summarize [topic]`

保存されたレコードをLLMが要約します。トピック・タグ・日付を指定して対象を絞れます。

**使いどころ：** 週次レビュー、インシデントの振り返り、営業パイプラインのまとめなど、定期的なダイジェスト作成に。大量のレコードを手動で読む代わりに、主要なテーマとパターンを一段落で把握できます。

```bash
idb summarize "顧客フィードバック" --tag support --after 2024-06-01
idb summarize "デプロイ障害" --tag incident
idb summarize    # 全レコードの総まとめ
```

---

### `idb cluster --k <N>`

エンベッドに対してk-meansクラスタリングを実行し、意味的に近いレコードをN個のグループに自動分類します。異なる言葉で書かれていても、意味が近ければ同じグループになります。

**使いどころ：** データが増えてきたときの全体像の把握、ナレッジベースの整理、タグ設計の参考に。「このデータセット、実はどんなトピックに分かれているんだろう？」という探索的な分析に向いています。

```bash
idb cluster --k 5                 # 全レコードを5つのグループに分類
idb cluster --k 3 --tag support   # サポートチケットを3つのテーマに分類
```

---

### `idb list`

保存順に全レコードを表示します。タグで絞り込むことも可能です。

**使いどころ：** 何が保存されているかを確認したいとき、特定タグのレコードを棚卸ししたいとき。`search` と違い、ランキングなしで全件返します。

```bash
idb list
idb list --tag urgent
idb --ns sales list
```

---

### `idb timeline`

保存されたユーザープロンプトとClaudeの回答を、時系列で交互に表示します。Claude Codeのフックで自動保存された会話履歴を確認するのに便利です。

レコードは自動的に分類されます：
- **[User]** — `UserPromptSubmit` フックで保存されたレコード（`hook_event_name: "UserPromptSubmit"` のJSON）
- **[Claude]** — `Stop` フックで保存されたレコード（`response` タグ付き）
- その他のレコードはデフォルトで非表示

**使いどころ：** 保存された会話履歴を振り返る、何が記録されているかデバッグする、特定セッションの流れを追う。

```bash
idb timeline                        # 全セッションを古い順に表示
idb timeline --limit 20             # 最新20件を表示
idb timeline --session abc123       # セッションIDのプレフィックスで絞り込み
idb timeline --show-notes           # 未分類レコードも表示
```

HTTP APIでは `GET /timeline`、Web UIでは **Timeline** タブからも利用できます。

---

### `idb update <id> <text>`

既存レコードのテキストを置き換えます。新しいテキストは自動的に再エンベッドされるため、更新後も正確な検索が可能です。

**使いどころ：** 記録した情報が変化したとき。バグが修正された、商談の金額が変わった、決定事項が覆った、などのケースで古いレコードを更新します。IDは `list` や `search` の出力に表示される先頭8文字で指定できます。

```bash
idb update a3f9c2b1 "アリスがエンタープライズプランに移行 — 最終契約額は800万円"
idb update 7d2e4a0f "メモリリークをv2.3.1で修正・本番デプロイ済み" --tag fix --tag resolved
```

---

### `idb delete <id>`

IDプレフィックスでレコードを完全削除します。削除後はHNSWインデックスが自動再構築されます。

**使いどころ：** 誤入力したレコード、情報が古くなったレコード、手動で重複を削除したいときに。

```bash
idb delete a3f9c2b1
```

---

### `idb related <id>`

指定したレコードと意味的に似ているレコードを、類似度順で返します。

**使いどころ：** あるレコードを起点に、関連情報を芋づる式に探したいとき。類似する過去の障害事例、同じ顧客に関する他のメモ、関連する決定事項などを発見できます。

```bash
idb related a3f9c2b1 --top 10
```

---

### `idb dedup`

全レコードをスキャンし、意味的に近い重複ペアを検出します。`--delete` をつけると、ペアの新しい方を自動削除します。

**使いどころ：** 一括インポート後、または長期運用の後にデータを整理するとき。`--threshold` で厳しさを調整できます（0.99: ほぼ同一文のみ / 0.90: 言い回しが違う同内容も対象）。

```bash
idb dedup --threshold 0.97            # 重複ペアを確認
idb dedup --threshold 0.97 --delete   # 各ペアの新しい方を自動削除
```

---

### `idb import <ファイル | ->`

JSON配列・CSV・テキストファイル（1行1レコード）、または標準入力からレコードを一括登録します。各エントリのエンベッドは自動生成されます。

**使いどころ：** 既存データのマイグレーション（Notionエクスポート、顧客データCSV、過去のログアーカイブなど）。`-` を指定するとstdin読み込みになり、ログのリアルタイム取り込みにも使えます。

```bash
idb import customers.csv
idb import meeting_notes.json --tag meeting
cat raw_logs.txt | idb import - --tag log
```

---

### `idb export`

全レコードをJSONまたはCSVでエクスポートします（ベクトルデータは除外）。

**使いどころ：** 定期バックアップ、他ツールへのデータ連携、人間が読めるスナップショットの作成。

```bash
idb export --format json -o backup.json
idb export --format csv -o snapshot.csv
```

---

### `idb serve`

ローカルHTTPサーバーを起動します。CLIの全機能がREST APIとして利用可能になり、Webアプリやスクリプトとの統合が容易になります。

**使いどころ：** Pythonスクリプト、Webアプリ、CIパイプライン、社内ツールなどHTTPリクエストが使える環境からintentdbを利用したいとき。

```bash
idb serve --port 3000
idb serve --port 8080 --host 0.0.0.0   # 全インターフェースで公開
```

---

### 検索オプション一覧

| フラグ | デフォルト | 説明 |
|---|---|---|
| `--top N` | 5 | 返す件数 |
| `--tag <tag>` | — | このタグを持つレコードのみ対象（複数指定可） |
| `--after YYYY-MM-DD` | — | この日付以降に保存されたレコードのみ |
| `--before YYYY-MM-DD` | — | この日付以前に保存されたレコードのみ |
| `--alpha 0.0–1.0` | 1.0 | スコアのブレンド率: 1.0=意味的類似度のみ、0.0=キーワード一致のみ |
| `--min-score 0.0–1.0` | 0.0 | このスコア未満の結果を除外 |

---

## アーキテクチャ

intentdb は既存のデータベースへのラッパーではなく、専用のストレージエンジンとして設計されています。

```
┌─────────────────────────────────────────┐
│              CLI / HTTP API             │
├─────────────────────────────────────────┤
│         自然言語クエリエンジン            │
│     （クエリ → エンベッド → HNSW探索）  │
├─────────────────────────────────────────┤
│         HNSW インデックス（独自実装）    │
│    Hierarchical Navigable Small World   │
├─────────────────────────────────────────┤
│      カスタムファイル形式 (.idb)         │
│  [MAGIC][レコード数][vector + tags]...  │
└─────────────────────────────────────────┘
```

### .idb ファイルフォーマット

```
[MAGIC: 4B "IDB2"]
[レコード数: u32]
[レコード 1]
  [id長: u16][id bytes]
  [テキスト長: u32][テキスト bytes]
  [ベクトル次元数: u32][f32 × N]
  [タイムスタンプ: u64]
  [タグ数: u16]
    [タグ長: u16][タグ bytes] × N
[レコード 2] ...
```

HNSWグラフは `.hnsw` ファイルに別途保存されます（`.idb` と同じベース名）。欠落または不整合の場合は自動再構築されます。

SQLite・PostgreSQL・RocksDB などの既存ストレージエンジンには一切依存していません。

---

## ベンチマーク

Apple M2、1536次元ベクトル（OpenAI `text-embedding-3-small`）、M=16、ef=50での実測値。

| レコード数 | 線形スキャン | intentdb (HNSW) | 高速化倍率 |
|---------|-------------|-----------------|---------|
| 1,000   | ~8ms        | ~0.4ms          | ~20×    |
| 10,000  | ~80ms       | ~1.2ms          | ~67×    |
| 100,000 | ~820ms      | ~4.8ms          | ~170×   |

---

## ユースケース

- **プロンプトライブラリ** — タイトルではなく意味でAIプロンプトを保存・検索
- **個人知識ベース** — メモを自由に蓄積して意味的に検索
- **コードスニペット** — 「ファイルを1行ずつ読むやつ」で検索できる
- **顧客メモ** — スキーマ不要のCRM
- **エラーログ検索** — 「これに似た過去の障害」を探す
- **日次ログ** — 自由書式で記録、後から意味的に取り出す
- **ログパイプライン** — `tail -f app.log | idb import -` でリアルタイム蓄積
- **週次ダイジェスト** — `idb summarize "今週" --after 2024-01-01` で自動サマリー

---

## ロードマップ

- [x] カスタム `.idb` ファイル形式
- [x] HNSWインデックス（Rustでゼロから実装）
- [x] 自然言語 put / search / list / delete / update
- [x] メタデータ・タグフィルタリング
- [x] HTTP API
- [x] 一括インポート（JSON、CSV、TXT、stdin）
- [x] エクスポート（JSON、CSV）
- [x] 重複検出
- [x] 関連レコード探索
- [x] crates.io への公開 (`cargo install intentdb`)
- [x] Pythonクライアント（`python/intentdb.py`、標準ライブラリのみ）
- [x] Dockerイメージ
- [x] Ollama・ローカルLLM対応（`--embedding-url`、`--llm-url`）
- [x] `ask` コマンド — 保存レコードへのRAQ
- [x] `summarize` コマンド — LLMによるレコード要約
- [x] `cluster` コマンド — 意味的k-meansグルーピング
- [x] タイムトラベルクエリ（`--before`、`--after`）
- [x] ハイブリッド検索（`--alpha` 意味的類似度 + キーワード）
- [x] 最小スコアフィルター（`--min-score`）
- [x] ネームスペース（`--ns`）
- [x] マルチデバイス同期（`idb sync push/pull`）
- [x] Web UI（`idb serve` で `http://localhost:3000/` に自動表示）
- [x] `timeline` コマンド — ユーザープロンプトとClaudeの回答を時系列表示
- [x] MCPサーバー（`idb mcp`） — Model Context Protocol経由のClaude Code統合
- [x] `GET /tags` エンドポイント — レコード数付きタグ一覧
- [x] Browse タグクラウド — クリックして即時フィルタ
- [x] Web UI Import タブ — JSON / CSV / TXT をブラウザから直接インポート

---

## コントリビュート

Issueやプルリクエストを歓迎します。  
intentdbが役に立ったと思ったら、ぜひ ⭐ をつけてください — 他のユーザーへの発見に繋がります。

```bash
git clone https://github.com/zzzzico12/intentdb
cd intentdb
cargo build
```

---

## ライセンス

MIT © zzzzico12
