# intentdb — Prompt History

> Claude Code の会話履歴をセマンティック検索できるビューア。  
> A semantic conversation log viewer for Claude Code.

[![CI](https://img.shields.io/github/actions/workflow/status/zzzzico12/intentdb/ci.yml?style=flat-square)](https://github.com/zzzzico12/intentdb/actions)
[![crates.io](https://img.shields.io/crates/v/intentdb?style=flat-square)](https://crates.io/crates/intentdb)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](LICENSE)

Claude Code のフックで会話を自動記録し、Web UI と CLI からセマンティック検索・閲覧できます。

```
Claude Code (hooks)
     │  UserPromptSubmit → prompt を保存
     │  Stop             → Claude の回答を保存
     ▼
  intentdb (.idb)
     │  HNSW インデックス（独自実装）
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
# ./target/release/idb が生成される
```

**Requirements:** Rust 1.75+、OpenAI API key（または Ollama）

---

## Setup: Claude Code hooks

`~/.claude/settings.json` に以下を追加します：

```json
{
  "hooks": {
    "UserPromptSubmit": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "jq -Rs '{text: ., tags: [\"prompt\"]}' | jq -s . | idb import - --format json"
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

`import_response.py` は本リポジトリに含まれています。パスを環境に合わせて書き換えてください。

設定後、Claude Code で会話するたびにプロンプトと回答が自動的に保存されます。

---

## Web UI

```bash
idb serve
```

`http://localhost:3000` でブラウザUIが開きます。

**タブ:**
- **Timeline** — セッション一覧。クリックで展開し会話を時系列表示。「Copy session as Markdown」でまとめてコピー
- **Search** — 自然言語でセマンティック検索。User / Claude / All でロールフィルター
- **Ask** — 会話履歴をコンテキストとしてLLMに質問（RAG）

---

## CLI

```bash
# タイムライン表示（User + Claude 時系列）
idb timeline
idb timeline --limit 20
idb timeline --session <session-id-prefix>

# セマンティック検索
idb search "Rustのライフタイムについて話した"
idb search "デプロイの設定" --top 10

# 会話履歴へのRAG質問
idb ask "先月どんな問題を解決しましたか？"

# レコード一覧
idb list
idb list --tag response

# サーバー起動
idb serve --port 3000

# MCP サーバー（Claude Code から直接利用）
idb mcp
```

---

## HTTP API

```bash
idb serve --port 3000
```

```bash
# タイムライン
curl "http://localhost:3000/timeline"
curl "http://localhost:3000/timeline?session=<id>&role=user&limit=50"

# セッション一覧（件数・最初のプロンプト付き）
curl "http://localhost:3000/timeline/sessions"

# セマンティック検索
curl "http://localhost:3000/search?q=Rustのエラー処理&top=5"

# タグ一覧（件数付き）
curl "http://localhost:3000/tags"

# RAG質問
curl -X POST http://localhost:3000/ask \
  -H "Content-Type: application/json" \
  -d '{"question": "先月の主な作業は？", "top": 5}'

# サマリー
curl "http://localhost:3000/summarize?topic=今週の作業"

# インポート（JSON / CSV / TXT）
curl -X POST "http://localhost:3000/ingest?format=json" \
  -H "Content-Type: text/plain" \
  -d '[{"text": "メモ", "tags": ["note"]}]'
```

---

## MCP server

Claude Code から intentdb をツールとして使う場合は `idb mcp` を使います。

`~/.claude/settings.json` に追加：

```json
{
  "mcpServers": {
    "intentdb": {
      "command": "idb",
      "args": ["mcp"]
    }
  }
}
```

利用可能なツール: `put` / `search` / `ask` / `list` / `summarize` / `timeline`

---

## Ollama（ローカル・APIキー不要）

```bash
ollama pull nomic-embed-text
ollama pull llama3

export IDB_EMBEDDING_URL=http://localhost:11434/v1/embeddings
export IDB_EMBEDDING_MODEL=nomic-embed-text
export IDB_LLM_URL=http://localhost:11434/v1/chat/completions
export IDB_LLM_MODEL=llama3
```

| 環境変数 | CLI フラグ | デフォルト |
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

HNSW グラフは `.hnsw` ファイルに別途保存。欠落時は自動再構築。

---

## Roadmap

- [x] Custom `.idb` file format + HNSW index (from scratch)
- [x] Semantic search (put / search / ask / list / delete / update)
- [x] Tag filtering, time-travel queries, hybrid search, min-score filter
- [x] Bulk import (JSON / CSV / TXT / stdin)
- [x] Export (JSON / CSV)
- [x] HTTP API + Web UI
- [x] Multi-device sync (`idb sync push/pull`)
- [x] `summarize` — LLM summary of stored records
- [x] `cluster` — semantic k-means grouping
- [x] Namespaces (`--ns`)
- [x] Ollama / local LLM support
- [x] MCP server (`idb mcp`)
- [x] `timeline` — Claude Code conversation log viewer (User + Claude interleaved)
- [x] Session card view in Web UI (accordion expand, copy as Markdown)
- [x] Role filter in Search (User / Claude / All)
- [x] `GET /tags` endpoint + tag cloud in Web UI

---

## Capture: other AI tools

`capture/` ディレクトリに他のAIツールとの連携ライブラリがあります。

### Python wrapper（OpenAI / Anthropic SDK）

```python
# OpenAI / Gemini / Mistral / Ollama など OpenAI互換API
import openai
from capture.capture import IdbCapture

client = IdbCapture(openai.OpenAI())
resp = client.chat.completions.create(
    model="gpt-4o",
    messages=[{"role": "user", "content": "Rustのライフタイムを教えて"}]
)
# → 自動的に intentdb に保存される

# Anthropic SDK
import anthropic
from capture.capture import IdbCaptureAnthropic

client = IdbCaptureAnthropic(anthropic.Anthropic())
resp = client.messages.create(
    model="claude-opus-4-5",
    max_tokens=1024,
    messages=[{"role": "user", "content": "Rustのライフタイムを教えて"}]
)

# 単体保存（任意のツール）
from capture.capture import save_conversation
save_conversation(prompt="質問", response="回答", tags=["my-tool"])
```

### Shell function（CLI AI ツール）

```bash
# .zshrc / .bashrc に追加
source /path/to/capture/idb_capture.sh

# llm (https://llm.datasette.io/) で質問 + 自動保存
ai "Dockerfileのベストプラクティスを教えて"

# 他のCLIツールを使う場合
export AI_CMD="sgpt"    # Shell GPT
export AI_CMD="aichat"  # aichat
ai "質問をここに"

# 任意コマンドをラップ
idb_wrap sgpt "Pythonの型ヒントを説明して"

# セッションを新しく開始
idb_new_session
```

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
