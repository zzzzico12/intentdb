#!/usr/bin/env python3
"""
Claude Code Stop hook — Claudeの最後の回答をidbに登録する
stdin: {"session_id": "...", "transcript_path": "/path/to/transcript.jsonl", ...}
"""
import json, sys, subprocess
from datetime import datetime

LOG = "/tmp/idb_stop_hook.log"

def log(msg):
    with open(LOG, "a") as f:
        f.write(f"{datetime.now().isoformat()} {msg}\n")

raw = sys.stdin.read()
log(f"stdin={raw[:200]}")

try:
    data = json.loads(raw)
except Exception as e:
    log(f"json parse error: {e}")
    sys.exit(0)
if data.get("stop_hook_active"):
    log("stop_hook_active, skipping")
    sys.exit(0)  # 無限ループ防止

transcript_path = data.get("transcript_path", "")
log(f"transcript_path={transcript_path}")
if not transcript_path:
    sys.exit(0)

# JSONLから最後のend_turn assistantメッセージを取得
# 各行は {"type": "assistant", "message": {"role": "assistant", "content": [...], "stop_reason": "end_turn", ...}}
last_text = ""
try:
    with open(transcript_path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            obj = json.loads(line)
            if obj.get("type") != "assistant":
                continue
            msg = obj.get("message", {})
            if msg.get("stop_reason") != "end_turn":
                continue
            content = msg.get("content", [])
            if isinstance(content, list):
                parts = [b["text"] for b in content if b.get("type") == "text"]
                if parts:
                    last_text = "\n".join(parts)
except Exception as e:
    log(f"parse error: {e}")
    sys.exit(0)

if not last_text.strip():
    log("no assistant text found")
    sys.exit(0)

log(f"importing {len(last_text)} chars")
record_text = json.dumps({
    "hook_event_name": "Stop",
    "session_id": data.get("session_id", ""),
    "response": last_text,
}, ensure_ascii=False)
payload = json.dumps([{"text": record_text, "tags": ["response"]}])
subprocess.run(
    ["/Users/otsuka/Documents/db/target/release/idb",
     "--file", "/Users/otsuka/Documents/db/data.idb",
     "import", "-", "--format", "json"],
    input=payload.encode(),
    capture_output=True,
)
