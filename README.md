# intentdb

> A schema-free, intent-native storage engine. Put data in plain language. Search in plain language.

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
git clone https://github.com/zzzzico12/intentdb
cd intentdb
cargo build --release
# Add to PATH, or use ./target/release/idb directly
```

Set your OpenAI API key:

```bash
export OPENAI_API_KEY=sk-...
```

**Requirements:** Rust 1.75+, OpenAI API key

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

# Export (no vectors)
idb export --format json -o backup.json
idb export --format csv -o backup.csv
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

# Search
curl "http://localhost:3000/search?q=recent+sales&top=5"
curl "http://localhost:3000/search?q=bugs&top=5&tag=urgent"

# List
curl "http://localhost:3000/records"
curl "http://localhost:3000/records?tag=sales"

# Delete
curl -X DELETE http://localhost:3000/records/<id>
```

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

---

## Roadmap

- [x] Custom `.idb` file format
- [x] HNSW index (from scratch in Rust)
- [x] Natural language put / search / list / delete / update
- [x] Metadata & tag filtering
- [x] HTTP API
- [x] Bulk import (JSON, CSV, TXT)
- [x] Export (JSON, CSV)
- [x] Duplicate detection
- [x] Related record discovery
- [ ] `cargo install intentdb` on crates.io
- [ ] Python client (`intentdb-py`)
- [ ] Docker image
- [ ] Multi-device sync
- [ ] Web UI

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
