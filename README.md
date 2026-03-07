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
| doge-lotto | Full — deploys, atomic ticket mints, auto-resolution, Burners mechanic | `doghook lotto deploy`, `doghook lotto mint`, `doghook lotto list`, `doghook lotto status`, `doghook lotto burn`, `doghook lotto burners` |

### doge-lotto

`doghook lotto mint` broadcasts one atomic transaction that:

- Pays the deploy's `prize_pool_address` the exact `ticket_price_koinu` amount.
- Optionally pays an immutable protocol developer tip in the same transaction.
- Inscribes the `doge-lotto` mint JSON in the same transaction.

This lets the indexer verify payment and tip commitments trustlessly.

Example deploy payload with explicit ticket cutoff (if omitted, defaults to `draw_block - 10`):

```bash
doghook lotto deploy \
   --type doge-69-420 \
   --draw-block 6200000 \
   --cutoff-block 6199990 \
   --ticket-price-koinu 100000000 \
   --prize-pool-address Dxxxxxxxxxxxxxxxxxxxxxxxxxxxx \
   --fee-percent 0 \
   --resolution-mode closest_wins \
   --rollover-enabled \
   --json
```

**Note:** The `prize_pool_address` receives all ticket payments and holds prizes. Any unclaimed prizes after 30 days support ongoing protocol development (see Unclaimed Prizes section below).

#### Optional Immutable Tip To Protocol Developers

Mints can include `--tip <0-10>` (default `0`) to commit an immutable protocol-dev tip percentage.

- `tip_percent` is written into the mint inscription JSON.
- The same atomic mint transaction sends `ticket_price_koinu * tip_percent / 100` to `protocols.lotto.protocol_dev_address`.
- The committed tip percent is stored per ticket and applied if that ticket wins.
- Winner payouts are automatically reduced by the committed tip amount, and the deduction is recorded.

Example mint with a 5% immutable tip:

```bash
doghook lotto mint \
   --lotto doge-69-420 \
   --quickpick \
   --tip 5 \
   --config-path doghook.toml
```

Set the protocol dev destination in config:

```toml
[protocols.lotto]
enabled = true
burn_address = "DBurnXXXXXXXXXXXXXXXXXXXXXXX9eVvaA"
protocol_dev_address = "Dxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
```

#### Burners — Reward Losing Tickets

Users can transfer expired lottery tickets to the official burn address (`DBurnXXXXXXXXXXXXXXXXXXXXXXX9eVvaA` by default) to earn **Burn Points**.

- **1 burned ticket = 1 Burn Point**
- **Every 10 Burn Points = 1 automatic entry into the monthly "Burners Bonus Draw"**
- Bonus draws are funded separately (1% protocol fee or dedicated pool)
- Only tickets from **resolved or expired lotteries** can be burned

**Burn a ticket:**
```bash
# Get ticket info and burn address
doghook lotto burn <ticket-inscription-id> --config-path doghook.toml

# Then use your wallet to transfer the inscription to the burn address
```

**Check Burn Points:**
```bash
# View your own burn points
doghook lotto burners --address D1abc... --config-path doghook.toml

# View leaderboard (top 10 burners)
doghook lotto burners --config-path doghook.toml

# JSON output
doghook lotto burners --config-path doghook.toml --json
```

#### Unclaimed Prizes — Supporting Protocol Development

**The 30-Day Rule:**
- Prizes that remain unclaimed for **30 days after the draw block** are permanently considered donations to the protocol developers.
- The `prize_pool_address` (set during lottery deployment) permanently holds all ticket payments and unclaimed prizes.
- Winners have 30 days to claim their prizes by transferring the winning ticket inscription to their desired address.
- After 30 days, unclaimed funds remain in the prize pool address to support ongoing doghook development and infrastructure.

**For Protocol-Level Lotteries (`doge-69-420` and `doge-max`):**
- This is **explicit public policy** — all participants acknowledge that unclaimed prizes fund future development.
- The protocol developers maintain the prize pool wallets for these official lotteries.
- Transparency: all prize pool addresses are publicly visible on-chain and in deploy inscriptions.

**For Community/Mini Lotteries:**
- Deployers set their own `prize_pool_address` and manage unclaimed funds according to their stated rules.
- The 30-day window is a recommended best practice but enforced at the social/community level.

**Check Prize Status:**
```bash
# View lottery status including unclaimed prizes
doghook lotto status <lotto-id> --config-path doghook.toml
```

The `lotto status` command shows:
- Total prize pool and net prizes awarded
- List of unclaimed wins with their age (days since draw)
- Clear indication when prizes enter the unclaimed/development fund after 30 days

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
