#!/usr/bin/env python3
"""
Claude Code UserPromptSubmit hook — ユーザーのプロンプトをリアルタイムでidbに登録する
stdin: {"prompt": "...", "session_id": "...", "cwd": "..."}

idb serve (localhost:3000) に直接 POST することで
OPENAI_API_KEY をサブプロセス環境に渡す必要をなくしている。
"""
import json, sys, urllib.request, urllib.error
from datetime import datetime

LOG = "/tmp/idb_prompt_hook.log"
IDB_URL = "http://localhost:3000"


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

import re

prompt = data.get("prompt", "")
session_id = data.get("session_id", "")
cwd = data.get("cwd", "")

# <ide_opened_file>...</ide_opened_file> などのシステムコンテキストタグを除去
prompt = re.sub(r"<[a-z_]+>.*?</[a-z_]+>\s*", "", prompt, flags=re.DOTALL).strip()

if not prompt.strip():
    sys.exit(0)

record = {
    "text": json.dumps(
        {"hook_event_name": "UserPromptSubmit", "prompt": prompt, "session_id": session_id, "cwd": cwd},
        ensure_ascii=False,
    ),
    "tags": ["prompt", "claude-code", "realtime"],
}

payload = json.dumps(record, ensure_ascii=False).encode()
req = urllib.request.Request(
    f"{IDB_URL}/records",
    data=payload,
    headers={"Content-Type": "application/json"},
)
try:
    urllib.request.urlopen(req, timeout=5)
    log(f"saved prompt ({len(prompt)} chars) session={session_id}")
except urllib.error.URLError as e:
    log(f"POST failed: {e} — is idb serve running?")
except Exception as e:
    log(f"unexpected error: {e}")
