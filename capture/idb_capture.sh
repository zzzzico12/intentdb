#!/usr/bin/env bash
# idb_capture.sh — CLI AI ツールの会話を intentdb に自動保存するシェル関数
#
# セットアップ:
#   source /path/to/idb_capture.sh     # .zshrc / .bashrc に追加
#
# 環境変数:
#   IDB_URL      intentdb サーバー URL (default: http://localhost:3000)
#   IDB_SESSION  セッションID。未設定なら source 時に自動生成。
#                手動でセッションを切り替えたい場合: export IDB_SESSION=$(uuidgen)
#
# 使い方:
#   ai "Rustのライフタイムを教えて"          # llm コマンドで質問 + 自動保存
#   ai "デプロイ手順を教えて" | less          # パイプにも対応
#   IDB_SESSION=$(uuidgen) ai "新しい話題"   # セッションを新規作成

# セッションIDを初期化（source 時に一度だけ生成）
if [[ -z "$IDB_SESSION" ]]; then
  if command -v uuidgen &>/dev/null; then
    export IDB_SESSION=$(uuidgen | tr '[:upper:]' '[:lower:]')
  else
    export IDB_SESSION=$(python3 -c 'import uuid; print(uuid.uuid4())')
  fi
fi

IDB_URL="${IDB_URL:-http://localhost:3000}"

# ── 内部: intentdb に保存 ─────────────────────────────────────────────────────
_idb_save() {
  local prompt="$1"
  local response="$2"
  local session_id="${IDB_SESSION}"
  local idb_url="${IDB_URL}"

  python3 - <<PYEOF
import sys, json, urllib.request

prompt   = ${prompt@Q}
response = ${response@Q}
session  = ${session_id@Q}
idb_url  = ${idb_url@Q}

def post(text, tags):
    data = json.dumps({"text": text, "tags": tags}, ensure_ascii=False).encode()
    req  = urllib.request.Request(
        f"{idb_url}/records", data=data,
        headers={"Content-Type": "application/json"},
    )
    try:
        urllib.request.urlopen(req, timeout=5)
    except Exception:
        pass

post(json.dumps({"hook_event_name": "UserPromptSubmit", "prompt": prompt,   "session_id": session}, ensure_ascii=False), ["prompt"])
post(json.dumps({"hook_event_name": "Stop",             "response": response, "session_id": session}, ensure_ascii=False), ["response"])
PYEOF
}

# ── ai: llm コマンドのラッパー ────────────────────────────────────────────────
# https://llm.datasette.io/ をインストールして使う
# 他のツールに変えたい場合は AI_CMD を設定:
#   export AI_CMD="sgpt"     # Shell GPT
#   export AI_CMD="aichat"   # aichat
ai() {
  local prompt="$*"
  if [[ -z "$prompt" ]]; then
    echo "Usage: ai <prompt>" >&2
    return 1
  fi

  local cmd="${AI_CMD:-llm}"
  if ! command -v "$cmd" &>/dev/null; then
    echo "Error: '$cmd' not found. Install it or set AI_CMD to your CLI AI tool." >&2
    return 1
  fi

  local response
  response=$("$cmd" "$prompt")
  local exit_code=$?

  echo "$response"

  if [[ $exit_code -eq 0 && -n "$response" ]]; then
    _idb_save "$prompt" "$response"
  fi

  return $exit_code
}

# ── idb_wrap: 任意コマンドをラップ ───────────────────────────────────────────
# Usage: idb_wrap <command> [args...]
#   コマンドの最後の引数を prompt、stdout を response として保存する
#
# 例:
#   idb_wrap sgpt "Dockerfileの書き方を教えて"
#   idb_wrap aichat "Pythonの型ヒントを説明して"
idb_wrap() {
  if [[ $# -lt 2 ]]; then
    echo "Usage: idb_wrap <command> <prompt...>" >&2
    return 1
  fi

  local cmd="$1"
  shift
  local prompt="$*"

  local response
  response=$("$cmd" "$@")
  local exit_code=$?

  echo "$response"

  if [[ $exit_code -eq 0 && -n "$response" ]]; then
    _idb_save "$prompt" "$response"
  fi

  return $exit_code
}

# ── idb_new_session: セッションを新しく開始 ──────────────────────────────────
idb_new_session() {
  if command -v uuidgen &>/dev/null; then
    export IDB_SESSION=$(uuidgen | tr '[:upper:]' '[:lower:]')
  else
    export IDB_SESSION=$(python3 -c 'import uuid; print(uuid.uuid4())')
  fi
  echo "New session: $IDB_SESSION"
}
