# doghook — Dogecoin / Doginals Indexer

The fastest, lightest, most selective Doginals indexer for Dogecoin.
Backward-traversal + reorg-safe (Chainhook engine) + Hiro-style predicate filtering.

Built as the modern successor to the original `dog` ord fork.

## Features

- Backward provenance tracing (like Ordhook)
- Full reorg safety via ZMQ + apply/rollback
- Selective indexing via `[doginals.predicates]` (only index dog-themed stuff, specific mime types, etc.)
- Native Dogecoin support (no SegWit/Taproot assumptions, koinu tracking, Dunes-ready)
- Postgres output + multi-protocol ready (Doginals + DRC-20 + Dunes)

## Quick Start

1. Run a Dogecoin Core node with:

   ```bash
   -txindex=1 -zmqpubrawblock=tcp://127.0.0.1:28332 -zmqpubrawtx=tcp://127.0.0.1:28332
   ```

2. Set env vars:

   ```bash
   export DOGE_RPC_USERNAME=youruser
   export DOGE_RPC_PASSWORD=yourpass
   ```

3. Edit `doghook.toml` (optional — predicates example included).

4. Run:

   ```bash
   cargo run --bin doghook -- --config doghook.toml
   ```

## Predicate Filtering (Hiro-style)

Enable in `doghook.toml`:

```toml
[doginals.predicates]
enabled = true
mime_types = ["image/png", "text/plain"]
content_prefixes = ["dog", "shibe", "woof"]
```

Only matching inscriptions are indexed. When disabled (default), indexes everything.

## Why doghook?

- Much faster and lighter than forward-only ord forks
- Selective = index only what you care about
- Production-ready reorg handling (critical on Dogecoin's 1-minute blocks)

Made with love for the Doge community.
