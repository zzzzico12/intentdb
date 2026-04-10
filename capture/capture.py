"""
intentdb capture wrapper — saves AI conversations to intentdb automatically.

Supports:
  - OpenAI SDK  (and any OpenAI-compatible API: Gemini, Mistral, Ollama, etc.)
  - Anthropic SDK

Usage (OpenAI):
    import openai
    from capture import IdbCapture

    client = IdbCapture(openai.OpenAI())
    resp = client.chat.completions.create(
        model="gpt-4o",
        messages=[{"role": "user", "content": "Rustのライフタイムを教えて"}]
    )
    # → prompt + response が intentdb に自動保存される

Usage (Anthropic):
    import anthropic
    from capture import IdbCaptureAnthropic

    client = IdbCaptureAnthropic(anthropic.Anthropic())
    resp = client.messages.create(
        model="claude-opus-4-5",
        max_tokens=1024,
        messages=[{"role": "user", "content": "Rustのライフタイムを教えて"}]
    )

Usage (standalone — any response):
    from capture import save_conversation
    save_conversation(prompt="...", response="...", tags=["my-tool"])

Environment variables:
    IDB_URL      intentdb server URL (default: http://localhost:3000)
    IDB_SESSION  session ID to group conversations (auto-generated if not set)
"""

import json
import os
import uuid
import urllib.request
import urllib.error
from typing import Optional

IDB_URL = os.environ.get("IDB_URL", "http://localhost:3000")


def save_conversation(
    prompt: str,
    response: str,
    tags: list[str] | None = None,
    session_id: str | None = None,
    idb_url: str = IDB_URL,
) -> None:
    """Save a prompt/response pair to intentdb."""
    session_id = session_id or os.environ.get("IDB_SESSION") or str(uuid.uuid4())
    tags = tags or []

    def _post(text: str, record_tags: list[str]) -> None:
        data = json.dumps({"text": text, "tags": record_tags}, ensure_ascii=False).encode()
        req = urllib.request.Request(
            f"{idb_url}/records",
            data=data,
            headers={"Content-Type": "application/json"},
        )
        try:
            urllib.request.urlopen(req, timeout=5)
        except Exception:
            pass  # intentdb が起動していなくても元の呼び出しを妨げない

    _post(
        json.dumps(
            {"hook_event_name": "UserPromptSubmit", "prompt": prompt, "session_id": session_id},
            ensure_ascii=False,
        ),
        ["prompt"] + tags,
    )
    _post(
        json.dumps(
            {"hook_event_name": "Stop", "response": response, "session_id": session_id},
            ensure_ascii=False,
        ),
        ["response"] + tags,
    )


# ── OpenAI-compatible wrapper ─────────────────────────────────────────────────

class IdbCapture:
    """
    OpenAI SDK（および互換API）のラッパー。
    chat.completions.create() を透過的にラップして会話を自動保存する。

    対応API:
      - OpenAI (GPT-4o, etc.)
      - Google Gemini (openai互換エンドポイント)
      - Mistral, Groq, Together AI, etc.
      - Ollama (openai互換モード)
    """

    def __init__(
        self,
        client,
        idb_url: str = IDB_URL,
        tags: list[str] | None = None,
        session_id: str | None = None,
    ):
        self._client = client
        self._idb_url = idb_url
        self._tags = tags or []
        self._session_id = session_id or str(uuid.uuid4())
        self.chat = _ChatProxy(self)

    def __getattr__(self, name):
        # chat 以外の属性は元クライアントに委譲
        return getattr(self._client, name)

    def _save(self, prompt: str, response: str) -> None:
        save_conversation(prompt, response, self._tags, self._session_id, self._idb_url)


class _ChatProxy:
    def __init__(self, capture: "IdbCapture"):
        self._capture = capture
        self.completions = _CompletionsProxy(capture)

    def __getattr__(self, name):
        return getattr(self._capture._client.chat, name)


class _CompletionsProxy:
    def __init__(self, capture: "IdbCapture"):
        self._capture = capture

    def create(self, messages: list[dict], **kwargs):
        # 最後のユーザーメッセージを prompt として取得
        prompt = ""
        for m in reversed(messages):
            if m.get("role") == "user":
                content = m.get("content", "")
                prompt = content if isinstance(content, str) else json.dumps(content)
                break

        resp = self._capture._client.chat.completions.create(messages=messages, **kwargs)

        # レスポンステキストを取得
        response = ""
        if resp.choices:
            response = resp.choices[0].message.content or ""

        self._capture._save(prompt, response)
        return resp

    def __getattr__(self, name):
        return getattr(self._capture._client.chat.completions, name)


# ── Anthropic SDK wrapper ─────────────────────────────────────────────────────

class IdbCaptureAnthropic:
    """
    Anthropic SDK のラッパー。
    messages.create() を透過的にラップして会話を自動保存する。
    """

    def __init__(
        self,
        client,
        idb_url: str = IDB_URL,
        tags: list[str] | None = None,
        session_id: str | None = None,
    ):
        self._client = client
        self._idb_url = idb_url
        self._tags = tags or []
        self._session_id = session_id or str(uuid.uuid4())
        self.messages = _AnthropicMessagesProxy(self)

    def __getattr__(self, name):
        return getattr(self._client, name)

    def _save(self, prompt: str, response: str) -> None:
        save_conversation(prompt, response, self._tags, self._session_id, self._idb_url)


class _AnthropicMessagesProxy:
    def __init__(self, capture: "IdbCaptureAnthropic"):
        self._capture = capture

    def create(self, messages: list[dict], **kwargs):
        # 最後のユーザーメッセージを取得
        prompt = ""
        for m in reversed(messages):
            if m.get("role") == "user":
                content = m.get("content", "")
                if isinstance(content, list):
                    # content block 形式
                    parts = [b.get("text", "") for b in content if b.get("type") == "text"]
                    prompt = "\n".join(parts)
                else:
                    prompt = str(content)
                break

        resp = self._capture._client.messages.create(messages=messages, **kwargs)

        # レスポンステキストを取得
        response = ""
        for block in resp.content:
            if hasattr(block, "text"):
                response += block.text

        self._capture._save(prompt, response)
        return resp

    def __getattr__(self, name):
        return getattr(self._capture._client.messages, name)
