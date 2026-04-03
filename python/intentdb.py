"""
intentdb Python client
Wraps the intentdb HTTP API (idb serve).

Usage:
    from intentdb import Client

    db = Client("http://localhost:3000")
    db.put("Alice closed a $50k deal on Friday", tags=["sales"])
    results = db.search("recent sales", top=5)
    for r in results:
        print(f"[{r['score']:.3f}] {r['text']}")
"""

from __future__ import annotations
import urllib.request
import urllib.parse
import json
from typing import Any


class Client:
    def __init__(self, base_url: str = "http://localhost:3000"):
        self.base_url = base_url.rstrip("/")

    def _request(self, method: str, path: str, body: Any = None, params: dict | None = None) -> Any:
        url = self.base_url + path
        if params:
            # list values (tag=[]) → repeated query params
            parts = []
            for k, v in params.items():
                if isinstance(v, list):
                    for item in v:
                        parts.append(f"{urllib.parse.quote(k)}={urllib.parse.quote(str(item))}")
                elif v is not None:
                    parts.append(f"{urllib.parse.quote(k)}={urllib.parse.quote(str(v))}")
            if parts:
                url += "?" + "&".join(parts)

        data = json.dumps(body).encode() if body is not None else None
        headers = {"Content-Type": "application/json", "Accept": "application/json"}
        req = urllib.request.Request(url, data=data, headers=headers, method=method)
        with urllib.request.urlopen(req) as resp:
            return json.loads(resp.read())

    def put(self, text: str, tags: list[str] | None = None) -> dict:
        """Add a record. Returns {id, text, tags, total}."""
        return self._request("POST", "/records", {"text": text, "tags": tags or []})

    def search(self, query: str, top: int = 5, tags: list[str] | None = None) -> list[dict]:
        """Semantic search. Returns list of {score, id, text, tags, timestamp}."""
        params: dict = {"q": query, "top": top}
        if tags:
            params["tag"] = tags
        return self._request("GET", "/search", params=params)

    def list(self, tags: list[str] | None = None) -> list[dict]:
        """List all records. Returns list of {id, text, tags, timestamp}."""
        params: dict = {}
        if tags:
            params["tag"] = tags
        return self._request("GET", "/records", params=params or None)

    def update(self, id: str, text: str, tags: list[str] | None = None) -> dict:
        """Update a record by id prefix. Returns updated {id, text, tags, timestamp}."""
        body: dict = {"text": text}
        if tags is not None:
            body["tags"] = tags
        return self._request("PATCH", f"/records/{id}", body)

    def delete(self, id: str) -> dict:
        """Delete a record by id prefix. Returns {deleted, remaining}."""
        return self._request("DELETE", f"/records/{id}")

    def related(self, id: str, top: int = 5) -> list[dict]:
        """Find semantically related records. Returns list of {score, id, text, tags, timestamp}."""
        return self._request("GET", f"/records/{id}/related", params={"top": top})

    def dedup(self, threshold: float = 0.95) -> list[dict]:
        """Detect duplicate pairs. Returns list of {score, a, b}."""
        return self._request("GET", "/dedup", params={"threshold": threshold})
