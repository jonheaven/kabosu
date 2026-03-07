# doghook — Dogecoin Indexer

The fastest, lightest, most selective Doginals indexer for Dogecoin.
Backward-traversal + reorg-safe (Chainhook engine) + Hiro-style predicate filtering + real-time webhooks.

## dog vs doghook

| | **dog** | **doghook** |
|---|---|---|
| Purpose | Full explorer / CLI / wallet | Blazing predicate backend + real-time hooks |
| Storage | redb (embedded, single-process) | Postgres (multi-client, production-grade) |
| Traversal | Forward (carry koinu ranges forward) | Backward (trace to coinbase on reveal) |
| Reorg safety | Manual | Automatic ZMQ apply/rollback |
| Selective indexing | No | Yes — MIME type + content prefix predicates |
| DNS / Dogemap / doge-lotto | Query only | Indexed natively, queryable via CLI + webhooks |
| Real-time hooks | No | Yes — POST JSON on every DNS/Dogemap event |
| Use when | You want a local explorer or wallet | You're building an app, API, or analytics pipeline |

Both projects are completely independent codebases. doghook does not import dog.

## Supported Protocols

| Protocol | Status | CLI commands |
|---|---|---|
| Doginals (inscriptions) | Full — backward traversal, reorg-safe | `doghook doginals service start` |
| DRC-20 | Full | via `doginals` indexer |
| Dunes | Full | `doghook dunes service start` |
| DNS (Dogecoin Name System) | Full — 28 namespaces, first-wins, reorg-safe | `doghook dns resolve`, `doghook dns list` |
| Dogemap (block claims) | Full — first-wins, reorg-safe | `doghook dogemap status`, `doghook dogemap list` |
| doge-lotto | Full — deploys, atomic ticket mints, auto-resolution | `doghook lotto deploy`, `doghook lotto mint`, `doghook lotto list`, `doghook lotto status` |

### doge-lotto

`doghook lotto mint` now broadcasts one atomic transaction that both pays the deploy's `prize_pool_address` the exact `ticket_price_koinu` amount and inscribes the `doge-lotto` ticket JSON in the same tx, so the indexer can verify payment trustlessly.

### DNS — Dogecoin Name System

Inscriptions whose body is `label.namespace` (e.g. `satoshi.doge`).
First inscription to register a name wins. Fully reorg-safe.

**Supported namespaces:** `.doge` `.dogecoin` `.shibe` `.shib` `.wow` `.very` `.such` `.much`
`.excite` `.woof` `.bark` `.tail` `.paws` `.paw` `.moon` `.kabosu`
`.cheems` `.inu` `.cook` `.doggo` `.boop` `.zoomies` `.smol` `.snoot` `.pupper` `.official`

```bash
# Resolve a name
doghook dns resolve satoshi.doge --config-path doghook.toml

# List all registered names
doghook dns list --config-path doghook.toml

# Filter by namespace
doghook dns list --namespace doge --config-path doghook.toml

# JSON output
doghook dns resolve satoshi.doge --config-path doghook.toml --json
```

### Dogemap (block claims)

Inscriptions whose body is `{N}.dogemap` (e.g. `1000.dogemap`). Blocks may only be claimed once.
First claim wins. Fully reorg-safe.

```bash
# Check if a block has been claimed
doghook dogemap status 1000000 --config-path doghook.toml

# List all claimed blocks
doghook dogemap list --config-path doghook.toml --limit 50

# JSON output
doghook dogemap status 1000000 --config-path doghook.toml --json
```

## Quick Start

1. Run a Dogecoin Core node with:

   ```bash
   -txindex=1 -zmqpubrawblock=tcp://127.0.0.1:28332 -zmqpubrawtx=tcp://127.0.0.1:28332
   ```

2. Set RPC credentials:

   ```bash
   export DOGE_RPC_USERNAME=youruser
   export DOGE_RPC_PASSWORD=yourpass
   ```

3. Copy and edit the config:

   ```bash
   cp doghook.toml my-indexer.toml
   # edit my-indexer.toml
   ```

4. Migrate the database:

   ```bash
   doghook doginals database migrate --config-path my-indexer.toml
   ```

5. Start indexing:

   ```bash
   doghook doginals service start --config-path my-indexer.toml
   ```

## Example Configurations

### Minimal — index everything

```toml
[storage]
working_dir = "data"

[dogecoin]
network    = "mainnet"
rpc_url    = "http://127.0.0.1:22555"
zmq_url    = "tcp://127.0.0.1:28332"

[doginals.db]
database = "doginals"
host     = "localhost"
port     = 5432
username = "postgres"
password = "postgres"

[resources]
ulimit                   = 2048
cpu_core_available       = 4
memory_available         = 8
dogecoin_rpc_threads     = 4
dogecoin_rpc_timeout     = 15
indexer_channel_capacity = 10
```

### NFT-only — images and text only

```toml
[doginals.predicates]
enabled         = true
mime_types      = ["image/png", "image/jpeg", "image/webp", "image/gif", "text/plain"]
```

### DNS + Dogemap only — skip all other inscriptions

```toml
[doginals.predicates]
enabled = false   # predicates off = index everything first

[protocols.dns]
enabled = true

[protocols.dogemap]
enabled = true
```

### Real-time webhooks — notify your app on every event

```toml
[webhooks]
enabled = true
urls    = ["https://api.yourapp.com/hooks/doghook"]
```

**DNS event payload:**
```json
{
  "event": "dns.registered",
  "name": "satoshi.doge",
  "inscription_id": "abc123i0",
  "block_height": 5000000,
  "block_timestamp": 1700000000
}
```

**Dogemap event payload:**
```json
{
  "event": "dogemap.claimed",
  "block_number": 1000000,
  "inscription_id": "def456i0",
  "claim_height": 5000001,
  "claim_timestamp": 1700000060
}
```

## Predicate Filtering (Hiro-style)

```toml
[doginals.predicates]
enabled         = true
mime_types      = ["image/png", "text/plain"]
content_prefixes = ["dog", "shibe", "woof"]
```

Only inscriptions matching **all** non-empty filter lists are stored. When disabled (default), every inscription is indexed. DNS and Dogemap detection always runs before the predicate filter — they are never accidentally excluded.

## Reorg Safety

Dogecoin produces 1-minute blocks and experiences more frequent reorgs than Bitcoin. doghook handles this automatically:

- ZMQ `rawblock` stream tracks the live chain tip
- On reorg detection: `rollback_block` removes inscription data, DNS registrations, Dogemap claims, and DRC-20 operations for each orphaned block — in a single Postgres transaction
- Replay continues from the fork point

## Building

```bash
cargo build --release
```

The binary is at `target/release/doghook`.

---

Made with love for the Doge community. Much index. Very fast. Wow.
