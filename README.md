# intentdb — Prompt History

> A semantic conversation log viewer for Claude Code.

[![CI](https://img.shields.io/github/actions/workflow/status/zzzzico12/intentdb/ci.yml?style=flat-square)](https://github.com/zzzzico12/intentdb/actions)
[![crates.io](https://img.shields.io/crates/v/intentdb?style=flat-square)](https://crates.io/crates/intentdb)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](LICENSE)

Automatically captures Claude Code conversations via hooks and makes them searchable through a Web UI, CLI, and MCP server.

```
Claude Code (hooks)
     │  UserPromptSubmit → saves prompt
     │  Stop             → saves Claude's response
     ▼
  idb serve (HTTP server)
     │  HNSW index (built from scratch)
     ▼
  Web UI  /  CLI  /  MCP server
```

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
# binary: ./target/release/idb
```

**Requirements:** Rust 1.75+, OpenAI API key (or Ollama for local inference)

---

## Setup

### 1. Start the server

The hooks POST to `idb serve`, so it must be running before any conversation is captured.

```bash
OPENAI_API_KEY=sk-... idb serve --file ~/intentdb/data.idb
```

**macOS — auto-start on login**

API keys should not be written directly into plist files. Use a wrapper script that reads from `~/.zshenv` instead.

**Step 1** — create `~/Library/LaunchAgents/idb_serve.sh`:

```bash
#!/bin/zsh
source ~/.zshenv
exec /usr/local/bin/idb \
  --file ~/intentdb/data.idb \
  serve --port 3000
```

```bash
chmod +x ~/Library/LaunchAgents/idb_serve.sh
```

**Step 2** — create `~/Library/LaunchAgents/com.intentdb.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>com.intentdb</string>
    <key>ProgramArguments</key>
    <array>
        <string>/Users/you/Library/LaunchAgents/idb_serve.sh</string>
    </array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>StandardOutPath</key><string>/tmp/idb_serve.log</string>
    <key>StandardErrorPath</key><string>/tmp/idb_serve.log</string>
</dict>
</plist>
```

```bash
launchctl load ~/Library/LaunchAgents/com.intentdb.plist
```

### 2. Configure Claude Code hooks

Add the following to `~/.claude/settings.json`:

```json
{
  "hooks": {
    "UserPromptSubmit": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "python3 /path/to/import_prompt.py"
          }
        ]
      }
    ],
    "Stop": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "python3 /path/to/import_response.py"
          }
        ]
      }
    ]
  }
}
```

Both `import_prompt.py` and `import_response.py` are included in this repository. Update the paths to match your environment.

**How the hooks work:**
- `import_prompt.py` — fires on every user message, strips IDE context tags (e.g. `<ide_opened_file>`), and POSTs to `http://localhost:3000/records`
- `import_response.py` — fires after each Claude response, reads the transcript, and POSTs the assistant reply to `http://localhost:3000/records`

Hooks POST to the running server rather than calling `idb` directly, so `OPENAI_API_KEY` only needs to be set once (in the server process). The hooks apply globally to all Claude Code sessions across all VSCode windows.

---

## Web UI

```bash
idb serve
```

Opens a browser UI at `http://localhost:3000`.

**Tabs:**
- **Timeline** — Session list. Click to expand and view the conversation chronologically. "Copy session as Markdown" to export.
- **Search** — Semantic search across all conversations. Filter by role: User / Claude / All.
- **Ask** — RAG: ask questions answered from your conversation history.

---

## CLI

```bash
# Interactive session list → select a session to view its timeline
idb list

# Timeline (User + Claude interleaved chronologically)
idb timeline
idb timeline --limit 20
idb timeline --session <session-id-prefix>

# Semantic search
idb search "talked about Rust lifetimes"
idb search "deployment config" --top 10

# RAG question over conversation history
idb ask "What problems did I solve last month?"

# Start server
idb serve --port 3000

# MCP server (for Claude Code integration)
idb mcp
```

---

## HTTP API

```bash
idb serve --port 3000
```

```bash
# Timeline
curl "http://localhost:3000/timeline"
curl "http://localhost:3000/timeline?session=<id>&role=user&limit=50"

# Session list (with message count and first prompt preview)
curl "http://localhost:3000/timeline/sessions"

# Semantic search
curl "http://localhost:3000/search?q=rust+error+handling&top=5"

# Tags with counts
curl "http://localhost:3000/tags"

# RAG question
curl -X POST http://localhost:3000/ask \
  -H "Content-Type: application/json" \
  -d '{"question": "What did I work on this week?", "top": 5}'

# Summarize
curl "http://localhost:3000/summarize?topic=this+week"

# Add a record
curl -X POST http://localhost:3000/records \
  -H "Content-Type: application/json" \
  -d '{"text": "note", "tags": ["note"]}'

# Import (JSON / CSV / TXT)
curl -X POST "http://localhost:3000/ingest?format=json" \
  -H "Content-Type: text/plain" \
  -d '[{"text": "note", "tags": ["note"]}]'
```

---

## MCP server

Works with **Claude Code** and **Claude Desktop**.

> **Note:** Claude Desktop enforces a maximum of 5 MCP tools per server.
> `timeline` and `summarize` are available via CLI/Web UI but not exposed over MCP for this reason.

### Claude Code

Add to `~/.claude/settings.json`:

```json
{
  "mcpServers": {
    "intentdb": {
      "command": "idb",
      "args": ["--file", "/path/to/data.idb", "mcp"]
    }
  }
}
```

### Claude Desktop (macOS)

API keys are not inherited by GUI apps — use a wrapper script.

**`~/Library/LaunchAgents/idb_mcp.sh`:**
```bash
#!/bin/zsh
source ~/.zshenv
exec /path/to/idb --file /path/to/data.idb mcp
```

**`~/Library/Application Support/Claude/claude_desktop_config.json`:**
```json
{
  "mcpServers": {
    "intentdb": {
      "command": "/Users/you/Library/LaunchAgents/idb_mcp.sh"
    }
  }
}
```

### Tools

| Tool | Description | Example use case |
|---|---|---|
| `put` | Store any text with optional tags | "Remember this decision for later" |
| `search` | Semantic search over stored records | "Find what I said about deployment" |
| `ask` | RAG: answer a question from stored records | "What did I decide about auth last week?" |
| `list` | List records, optionally filtered by tag | "Show me all records tagged `urgent`" |
| `log_conversation` | Save a user+assistant turn to the timeline | "Log this conversation to intentdb" |

#### put
```
Saves text to intentdb with semantic embedding.
Args: text (required), tags (optional list)
→ Use to store notes, decisions, instructions, or anything worth remembering.
```

#### search
```
Finds the most semantically similar records to a query.
Args: query (required), top (default: 5), tags (filter), alpha (1.0=semantic, 0.0=keyword)
→ Use when you want to retrieve relevant past context before answering.
```

#### ask
```
Answers a natural language question using stored records as context (RAG).
Args: question (required), top (default: 5 context records)
→ Use to surface insights from conversation history without knowing exact keywords.
```

#### list
```
Returns recent records, optionally filtered by tag.
Args: tags (filter), limit (default: 50)
→ Use to audit what's stored, or pull all records with a specific tag.
```

#### log_conversation
```
Saves a conversation turn (user message + Claude response) so it appears
in `idb list` and `idb timeline`. session_id is auto-generated per call.
Args: user_text (required), assistant_text (required)
→ Call this from Claude Desktop after each response to build a searchable timeline.
```

---

## Capture: other AI tools

The `capture/` directory contains wrappers for other AI tools.

### Python wrapper (OpenAI / Anthropic SDK)

```python
# OpenAI / Gemini / Mistral / Ollama and any OpenAI-compatible API
import openai
from capture.capture import IdbCapture

client = IdbCapture(openai.OpenAI())
resp = client.chat.completions.create(
    model="gpt-4o",
    messages=[{"role": "user", "content": "Explain Rust lifetimes"}]
)
# → automatically saved to intentdb

# Anthropic SDK
import anthropic
from capture.capture import IdbCaptureAnthropic

client = IdbCaptureAnthropic(anthropic.Anthropic())
resp = client.messages.create(
    model="claude-opus-4-5",
    max_tokens=1024,
    messages=[{"role": "user", "content": "Explain Rust lifetimes"}]
)

# Standalone (any tool)
from capture.capture import save_conversation
save_conversation(prompt="question", response="answer", tags=["my-tool"])
```

### Shell function (CLI AI tools)

```bash
# Add to .zshrc / .bashrc
source /path/to/capture/idb_capture.sh

# Ask using llm (https://llm.datasette.io/) + auto-save
ai "What are Dockerfile best practices?"

# Use a different CLI tool
export AI_CMD="sgpt"    # Shell GPT
export AI_CMD="aichat"  # aichat
ai "your question"

# Wrap any command
idb_wrap gh copilot explain "git rebase -i HEAD~3"

# Start a new session
idb_new_session
```

---

## Ollama (local, no API key needed)

```bash
ollama pull nomic-embed-text
ollama pull llama3

export IDB_EMBEDDING_URL=http://localhost:11434/v1/embeddings
export IDB_EMBEDDING_MODEL=nomic-embed-text
export IDB_LLM_URL=http://localhost:11434/v1/chat/completions
export IDB_LLM_MODEL=llama3
```

| Env var | CLI flag | Default |
|---|---|---|
| `OPENAI_API_KEY` | — | — |
| `IDB_EMBEDDING_URL` | `--embedding-url` | `https://api.openai.com/v1/embeddings` |
| `IDB_EMBEDDING_MODEL` | `--embedding-model` | `text-embedding-3-small` |
| `IDB_LLM_URL` | `--llm-url` | `https://api.openai.com/v1/chat/completions` |
| `IDB_LLM_MODEL` | `--llm-model` | `gpt-4o-mini` |

---

## Architecture

```
┌─────────────────────────────────────────┐
│     Web UI / CLI / HTTP API / MCP       │
├─────────────────────────────────────────┤
│         Natural Language Query Engine   │
│     (query → embedding → HNSW search)  │
├─────────────────────────────────────────┤
│         HNSW Index (from scratch)       │
├─────────────────────────────────────────┤
│      Custom File Format  (.idb)         │
│  [MAGIC][N records][vector + tags]...   │
└─────────────────────────────────────────┘
```

### .idb file format

```
[MAGIC: 4B "IDB2"][record count: u32]
[record]
  [id: u16 + bytes][text: u32 + bytes]
  [vector: u32 dims + f32×N][timestamp: u64]
  [tags: u16 count + (u16 + bytes)×N]
```

HNSW graph is stored separately in a `.hnsw` file. Rebuilt automatically if missing.

---

## Roadmap

- [x] Custom `.idb` file format + HNSW index (from scratch)
- [x] Semantic search (put / search / ask / list / delete / update)
- [x] Tag filtering, time-travel queries, hybrid search, min-score filter
- [x] Bulk import (JSON / CSV / TXT / stdin) + export (JSON / CSV)
- [x] HTTP API + Web UI
- [x] Multi-device sync (`idb sync push/pull`)
- [x] `summarize` / `cluster` / namespaces (`--ns`)
- [x] Ollama / local LLM support
- [x] MCP server (`idb mcp`)
- [x] `timeline` — conversation log viewer (User + Claude interleaved)
- [x] Session card view in Web UI (accordion, copy as Markdown)
- [x] Role filter in Search (User / Claude / All)
- [x] Python capture wrapper (OpenAI / Anthropic SDK)
- [x] Shell capture function (any CLI AI tool)
- [x] Interactive session list (`idb list`) with dialoguer
- [x] Hooks use HTTP POST to server (no API key needed in hook subprocess)

---

## Contributing

Issues and PRs welcome.  
If you find intentdb useful, please consider giving it a ⭐

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

# intentdb — Prompt History（日本語）

> Claude Code の会話履歴をセマンティック検索できるビューア。

Claude Code のフックで会話を自動記録し、Web UI と CLI からセマンティック検索・閲覧できます。

```
Claude Code (hooks)
     │  UserPromptSubmit → プロンプトを保存
     │  Stop             → Claudeの回答を保存
     ▼
  idb serve（HTTPサーバー）
     │  HNSWインデックス（独自実装）
     ▼
  Web UI  /  CLI  /  MCPサーバー
```

---

## インストール

```bash
cargo install intentdb
```

ソースからビルド：

```bash
git clone https://github.com/zzzzico12/intentdb
cd intentdb
cargo build --release
# バイナリ: ./target/release/idb
```

**動作要件：** Rust 1.75+、OpenAI APIキー（Ollamaを使えばローカルのみでも可）

---

## セットアップ

### 1. サーバーを起動する

フックは `idb serve` にPOSTするため、会話を記録するには事前にサーバーが起動している必要があります。

```bash
OPENAI_API_KEY=sk-... idb serve --file ~/intentdb/data.idb
```

**macOS — ログイン時に自動起動**

APIキーをplistに直書きしないよう、`~/.zshenv` から読み込むラッパースクリプト経由にします。

**Step 1** — `~/Library/LaunchAgents/idb_serve.sh` を作成：

```bash
#!/bin/zsh
source ~/.zshenv
exec /usr/local/bin/idb \
  --file ~/intentdb/data.idb \
  serve --port 3000
```

```bash
chmod +x ~/Library/LaunchAgents/idb_serve.sh
```

**Step 2** — `~/Library/LaunchAgents/com.intentdb.plist` を作成：

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>com.intentdb</string>
    <key>ProgramArguments</key>
    <array>
        <string>/Users/you/Library/LaunchAgents/idb_serve.sh</string>
    </array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>StandardOutPath</key><string>/tmp/idb_serve.log</string>
    <key>StandardErrorPath</key><string>/tmp/idb_serve.log</string>
</dict>
</plist>
```

```bash
launchctl load ~/Library/LaunchAgents/com.intentdb.plist
```

### 2. Claude Code フックを設定する

`~/.claude/settings.json` に以下を追加：

```json
{
  "hooks": {
    "UserPromptSubmit": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "python3 /path/to/import_prompt.py"
          }
        ]
      }
    ],
    "Stop": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "python3 /path/to/import_response.py"
          }
        ]
      }
    ]
  }
}
```

`import_prompt.py` と `import_response.py` はリポジトリに含まれています。パスを環境に合わせて書き換えてください。

**フックの動作：**
- `import_prompt.py` — ユーザーメッセージ送信時に発火。`<ide_opened_file>` などのIDEコンテキストタグを除去してから `http://localhost:3000/records` にPOST
- `import_response.py` — Claudeの返答後に発火。トランスクリプトから最新の回答を取り出してPOST

フックはCLIを直接呼ぶのではなく起動中のサーバーにPOSTするため、`OPENAI_API_KEY` はサーバー起動時に1回設定するだけでOKです。設定は全VSCodeウィンドウ・全プロジェクトに適用されます。

---

## Web UI

```bash
idb serve
```

`http://localhost:3000` でブラウザUIが開きます。

**タブ：**
- **Timeline** — セッション一覧。クリックで展開し会話を時系列表示。「Copy session as Markdown」でまとめてコピー
- **Search** — 自然言語でセマンティック検索。User / Claude / All でロールフィルター
- **Ask** — 会話履歴をコンテキストとしてLLMに質問（RAG）

---

## CLI

```bash
# インタラクティブなセッション一覧 → 選択してタイムライン表示
idb list

# タイムライン表示（User + Claude 時系列）
idb timeline
idb timeline --limit 20
idb timeline --session <session-id-prefix>

# セマンティック検索
idb search "Rustのライフタイムについて話した"
idb search "デプロイの設定" --top 10

# 会話履歴へのRAG質問
idb ask "先月どんな問題を解決しましたか？"

# サーバー起動
idb serve --port 3000

# MCPサーバー
idb mcp
```

---

## HTTP API

タイムライン・検索・RAG・インポートなど全機能がREST APIとして利用可能です。詳細は英語セクションを参照してください。

---

## MCPサーバー

**Claude Code** と **Claude Desktop** で利用できます。

> **注意：** Claude Desktop は1MCPサーバーあたり最大5ツールという制限があります。
> `timeline` と `summarize` はCLI/Web UIで利用可能ですが、この制限のためMCPには含まれていません。

### Claude Code

`~/.claude/settings.json` に追加：

```json
{
  "mcpServers": {
    "intentdb": {
      "command": "idb",
      "args": ["--file", "/path/to/data.idb", "mcp"]
    }
  }
}
```

### Claude Desktop（macOS）

GUIアプリはAPIキーを継承しないため、ラッパースクリプト経由にします。

**`~/Library/LaunchAgents/idb_mcp.sh`:**
```bash
#!/bin/zsh
source ~/.zshenv
exec /path/to/idb --file /path/to/data.idb mcp
```

**`~/Library/Application Support/Claude/claude_desktop_config.json`:**
```json
{
  "mcpServers": {
    "intentdb": {
      "command": "/Users/you/Library/LaunchAgents/idb_mcp.sh"
    }
  }
}
```

### ツール一覧

| ツール | 説明 | ユースケース例 |
|---|---|---|
| `put` | テキストをタグ付きで保存 | 「この決定を後で参照できるようにしておいて」 |
| `search` | セマンティック検索 | 「デプロイについて話したことを探して」 |
| `ask` | 会話履歴へのRAG質問 | 「先週の認証の決定は何だった？」 |
| `list` | レコード一覧（タグフィルター可） | 「`urgent` タグのレコードを全部見せて」 |
| `log_conversation` | 会話ターンをtimelineに保存 | 「この会話をidbに記録して」 |

詳細なパラメータは英語セクションを参照してください。

---

## 他のAIツールとの連携

`capture/` ディレクトリに他のAIツール用のラッパーがあります。

### Python wrapper（OpenAI / Anthropic SDK）

```python
# OpenAI / Gemini / Mistral / Ollama など OpenAI互換API
from capture.capture import IdbCapture
import openai

client = IdbCapture(openai.OpenAI())
resp = client.chat.completions.create(
    model="gpt-4o",
    messages=[{"role": "user", "content": "Rustのライフタイムを教えて"}]
)
# → 自動的にintentdbに保存される

# 単体保存（任意のツール）
from capture.capture import save_conversation
save_conversation(prompt="質問", response="回答", tags=["my-tool"])
```

### シェル関数（CLI AIツール）

```bash
# .zshrc / .bashrc に追加
source /path/to/capture/idb_capture.sh

# llm コマンドで質問 + 自動保存
ai "Dockerfileのベストプラクティスを教えて"

# 任意コマンドをラップ
idb_wrap gh copilot explain "git rebase -i HEAD~3"

# セッションを新規開始
idb_new_session
```

---

## ライセンス

MIT © zzzzico12
