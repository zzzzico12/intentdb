#!/usr/bin/env python3
"""
Claude Code Stop hook — 直近のuser→assistantペアをidbに登録する
stdin: {"session_id": "...", "transcript_path": "/path/to/transcript.jsonl", "cwd": "...", ...}

Stop hookは返答のたびに呼ばれるので、直近のペアのみ保存することで重複を防ぐ。
idb serve (localhost:3000) に直接 POST することで
OPENAI_API_KEY をサブプロセス環境に渡す必要をなくしている。

classify_record が認識するフォーマット:
  prompt  タグ + {"hook_event_name":"UserPromptSubmit","prompt":"...","session_id":"..."}
  response タグ + {"hook_event_name":"Stop","response":"...","session_id":"..."}
"""
import json, sys, urllib.request, urllib.error
from datetime import datetime

LOG = "/tmp/idb_stop_hook.log"
IDB_URL = "http://localhost:3000"


def log(msg):
    with open(LOG, "a") as f:
        f.write(f"{datetime.now().isoformat()} {msg}\n")


def post_record(text: str, tags: list) -> bool:
    payload = json.dumps({"text": text, "tags": tags}, ensure_ascii=False).encode()
    req = urllib.request.Request(
        f"{IDB_URL}/records",
        data=payload,
        headers={"Content-Type": "application/json"},
    )
    try:
        urllib.request.urlopen(req, timeout=10)
        return True
    except urllib.error.URLError as e:
        log(f"POST failed: {e} — is idb serve running?")
        return False
    except Exception as e:
        log(f"unexpected error: {e}")
        return False


def extract_text(content) -> str:
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = []
        for b in content:
            if isinstance(b, dict) and b.get("type") == "text":
                parts.append(b["text"])
        return "\n".join(parts)
    return ""


def get_last_pair(path: str):
    """transcript.jsonl から最後の user→assistant ペアを返す。"""
    user_turns = []
    assistant_turns = []

    try:
        with open(path) as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                obj = json.loads(line)
                role = obj.get("type", "")

                if role == "user":
                    msg = obj.get("message", {})
                    text = extract_text(msg.get("content", ""))
                    if text.strip():
                        ts = obj.get("timestamp", "")
                        user_turns.append((ts, text.strip()))

                elif role == "assistant":
                    msg = obj.get("message", {})
                    if msg.get("stop_reason") != "end_turn":
                        continue
                    text = extract_text(msg.get("content", []))
                    if text.strip():
                        ts = obj.get("timestamp", "")
                        assistant_turns.append((ts, text.strip()))

    except Exception as e:
        log(f"parse error: {e}")
        return None

    if not assistant_turns:
        log("no end_turn assistant message found")
        return None

    last_assistant_ts, last_assistant_text = assistant_turns[-1]

    last_user_text = ""
    for ts, text in reversed(user_turns):
        if ts <= last_assistant_ts or not last_assistant_ts:
            last_user_text = text
            break

    return last_user_text, last_assistant_text


# --- main ---
raw = sys.stdin.read()
log(f"stdin={raw[:200]}")

try:
    data = json.loads(raw)
except Exception as e:
    log(f"json parse error: {e}")
    sys.exit(0)

if data.get("stop_hook_active"):
    log("stop_hook_active, skipping")
    sys.exit(0)

transcript_path = data.get("transcript_path", "")
session_id = data.get("session_id", "")
cwd = data.get("cwd", "")
log(f"transcript_path={transcript_path}")

if not transcript_path:
    sys.exit(0)

pair = get_last_pair(transcript_path)
if pair is None:
    sys.exit(0)

user_text, assistant_text = pair
log(f"user={len(user_text)} chars, assistant={len(assistant_text)} chars")

# classify_record が認識するフォーマットで保存
text = json.dumps(
    {"hook_event_name": "Stop", "response": assistant_text, "session_id": session_id, "cwd": cwd},
    ensure_ascii=False,
)
ok = post_record(text, ["response", "claude-code"])
log(f"import {'ok' if ok else 'failed'}")
