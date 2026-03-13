
## Performance upgrades (Dogecoin Core-inspired)

Kabosu now includes a major performance pass inspired by Dogecoin Core internals:

- **Batched Postgres writes** for inscriptions, DMP, and DogeLotto paths (500-row chunks + `ON CONFLICT`).
- **Parallel indexing pipeline** architecture (reader/parser/filter/writer staging).
- **Connection pooling + hot prepared statement path** using `deadpool-postgres`.
- **In-memory hot caches** for recent blocks (last 1,000) and recent ownership movements.
- **Predicate pre-filtering** (quick MIME/content-prefix checks before expensive parsing).
- **Smart start height + checkpoint resume**:
  - defaults to **Doginals genesis `4609720`** on mainnet,
  - use `--index-rare-koinu` (or `--from 0`) to include the full chain,
  - redb checkpoint file enables instant resume after restart.

<p align="center">
  <img src="logo.png" alt="kabosu logo" />
</p>

# kabosu — Dogecoin Indexer

The fastest, lightest, most selective Doginals indexer for Dogecoin.
Backward-traversal + reorg-safe (Chainhook engine) + Hiro-style predicate filtering + real-time webhooks.

## dog vs kabosu

| | **dog** | **kabosu** |
|---|---|---|
| Purpose | Full explorer / CLI / wallet | Blazing predicate backend + real-time hooks |
| Storage | redb (embedded, single-process) | Postgres (multi-client, production-grade) |
| Traversal | Forward (carry koinu ranges forward) | Backward (trace to coinbase on reveal) |
| Reorg safety | Manual | Automatic ZMQ apply/rollback |
| Selective indexing | No | Yes — MIME type + content prefix predicates |
| DNS / Dogemap / DogeLotto | Query only | Indexed natively, queryable via CLI + webhooks |
| Real-time hooks | No | Yes — POST JSON on every DNS/Dogemap event |
| Use when | You want a local explorer or wallet | You're building an app, API, or analytics pipeline |

Both projects are completely independent codebases. kabosu does not import dog.

## Supported Protocols

| Protocol | Status | CLI commands |
|---|---|---|
| Doginals (inscriptions) | Full — backward traversal, reorg-safe | `kabosu doginals service start` |
| DRC-20 | Full | via `doginals` indexer |
| Dunes | Full | `kabosu dunes service start` |
| DNS (Dogecoin Name System) | Full — 28 namespaces, first-wins, reorg-safe | `kabosu dns resolve`, `kabosu dns list` |
| Dogemap (block claims) | Full — first-wins, reorg-safe | `kabosu dogemap status`, `kabosu dogemap list` |
| DogeLotto | Full — deploys, atomic ticket mints, auto-resolution, Burners mechanic | `kabosu lotto deploy`, `kabosu lotto mint`, `kabosu lotto list`, `kabosu lotto status`, `kabosu lotto burn`, `kabosu lotto burners`, `kabosu doginals index sync --only dogelotto` |
| Dogetag (on-chain graffiti) | Full — OP_RETURN text messages, reorg-safe | `kabosu dogetag list`, `kabosu dogetag search`, `kabosu dogetag address`, `kabosu dogetag send` |
| DogeSpells | Full — OP_RETURN magic-prefix + CBOR spells, balances, NFT metadata snapshots, reorg-safe | `kabosu doginals index sync --only dogespells` plus `/dogespells/*` API routes |
| DMP | Full — inscription-based marketplace: listings, bids, settlements, cancels; reorg-safe | `kabosu doginals index sync --only dmp`, `GET /api/dmp/listings` |

### DogeLotto

`kabosu lotto mint` broadcasts one atomic transaction that:

- Pays the deploy's `prize_pool_address` the exact `ticket_price_koinu` amount.
- Optionally pays an immutable protocol developer tip in the same transaction.
- Inscribes the `DogeLotto` mint JSON (with `"p":"DogeLotto"`) in the same transaction.

This lets the indexer verify payment and tip commitments trustlessly.

Run a dedicated DogeLotto-only sync:

```bash
kabosu doginals index sync --only dogelotto --config-path kabosu.toml
```

#### Lucky Ðraw → Ðeno (Keno-style)

- Players draw **Luck Marks**: pick `1-20` unique numbers from `1-80` by default (deploy-time configurable via `main_numbers.pick` and `main_numbers.max`).
- Designed for low activity: frequent draws are recommended (`100-500` blocks), small ticket price defaults, and broad winner coverage.
- Use `closest_wins` (default) for competitive scoring, or `always_winner` so at least one ticket receives payout each draw.
- Prize tiers naturally scale with participation: early draws stay fun with small pools, larger draws increase payout competitiveness.
- `kabosu lotto mint --lotto deno --quickpick` generates random Luck Marks and writes them into mint JSON as `luck_marks` (while retaining backward-compatible `seed_numbers` handling in the indexer).

Example deploy payload with explicit ticket cutoff (if omitted, defaults to `draw_block - 10`):

> Branding hierarchy: ecosystem = **Lucky Ðraw**, game = **Ðeno**, ticket mechanic = **Luck Marks**.

```bash
kabosu lotto deploy \
   --type deno \
   --draw-block 6200000 \
   --cutoff-block 6199990 \
   --ticket-price-koinu 1000000 \
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
kabosu lotto mint \
   --lotto deno \
   --quickpick \
   --tip 5 \
   --config-path kabosu.toml
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
kabosu lotto burn <ticket-inscription-id> --config-path kabosu.toml

# Then use your wallet to transfer the inscription to the burn address
```

**Check Burn Points:**
```bash
# View your own burn points
kabosu lotto burners --address D1abc... --config-path kabosu.toml

# View leaderboard (top 10 burners)
kabosu lotto burners --config-path kabosu.toml

# JSON output
kabosu lotto burners --config-path kabosu.toml --json
```

#### Unclaimed Prizes — Supporting Protocol Development

**The 30-Day Rule:**
- Prizes that remain unclaimed for **30 days after the draw block** are permanently considered donations to the protocol developers.
- The `prize_pool_address` (set during lottery deployment) permanently holds all ticket payments and unclaimed prizes.
- Winners have 30 days to claim their prizes by transferring the winning ticket inscription to their desired address.
- After 30 days, unclaimed funds remain in the prize pool address to support ongoing kabosu development and infrastructure.

**For Protocol-Level DogeLotto Deployments (`doge-69-420` and `doge-max`):**
- This is **explicit public policy** — all participants acknowledge that unclaimed prizes fund future development.
- The protocol developers maintain the prize pool wallets for these official lotteries.
- Transparency: all prize pool addresses are publicly visible on-chain and in deploy inscriptions.

**For Community/Mini Lotteries:**
- Deployers set their own `prize_pool_address` and manage unclaimed funds according to their stated rules.
- The 30-day window is a recommended best practice but enforced at the social/community level.

**Check Prize Status:**
```bash
# View lottery status including unclaimed prizes
kabosu lotto status <lotto-id> --config-path kabosu.toml
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
kabosu dns resolve satoshi.doge --config-path kabosu.toml

# List all registered names
kabosu dns list --config-path kabosu.toml

# Filter by namespace
kabosu dns list --namespace doge --config-path kabosu.toml

# JSON output
kabosu dns resolve satoshi.doge --config-path kabosu.toml --json
```

### Dogemap (block claims)

Inscriptions whose body is `{N}.dogemap` (e.g. `1000.dogemap`). Blocks may only be claimed once.
First claim wins. Fully reorg-safe.

```bash
# Check if a block has been claimed
kabosu dogemap status 1000000 --config-path kabosu.toml

# List all claimed blocks
kabosu dogemap list --config-path kabosu.toml --limit 50

# JSON output
kabosu dogemap status 1000000 --config-path kabosu.toml --json
```

Dogemap is its own inscription metaprotocol and is indexed independently of DNS, Dogetag, DRC-20, and DogeLotto.

### Dogetag — On-chain Graffiti

Any Dogecoin transaction that includes an `OP_RETURN` output with valid UTF-8 text (≤ 80 bytes) is a **Dogetag** — a permanent on-chain mark that lives in the blockchain's transaction history forever.

Dogetags are **not inscriptions**. They don't land in anyone's wallet. No backward traversal, no ownership, no koinu ranges. Just a message, burned into the chain.

Enable in config (default `true` when section is absent):

```toml
[protocols.dogetag]
enabled = true
```

**List recent tags:**
```bash
kabosu dogetag list --config-path kabosu.toml
kabosu dogetag list --limit 100 --json --config-path kabosu.toml
```

**Search by message content:**
```bash
kabosu dogetag search "much wow" --config-path kabosu.toml
kabosu dogetag search "satoshi" --json --config-path kabosu.toml
```

**Tags by address:**
```bash
kabosu dogetag address DYourAddressHere --config-path kabosu.toml
```

**Send DOGE + burn a message in the same tx:**
```bash
kabosu dogetag send \
  --to DRecipientAddressHere \
  --amount 5.0 \
  --message "such graffiti very chain wow" \
  --config-path kabosu.toml
```

**Webhook event payload:**
```json
{
  "event": "dogetag.tagged",
  "txid": "abc123...",
  "sender_address": "DYourAddress...",
  "message": "such graffiti very chain wow",
  "block_height": 5100000,
  "block_timestamp": 1700000000
}
```

Dogetag indexing tracks OP_RETURN UTF-8 graffiti messages from standard Dogecoin transactions and exposes them through CLI queries, web APIs, and webhooks.

### DogeSpells — OP_RETURN CBOR Spells

DogeSpells spells are **not inscriptions**. kabosu scans every OP_RETURN output, looks for the DogeSpells magic prefix, decodes the trailing CBOR spell, and indexes only spells whose `chain_id` is `doge`.

Enable in config (default `true` when section is absent):

```toml
[protocols.dogespells]
enabled = true
```

Run a dedicated DogeSpells-only sync:

```bash
kabosu doginals index sync --only dogespells --config-path kabosu.toml
```

Web/API routes:

```text
GET /dogespells/balance/:ticker/:address
GET /dogespells/history/:ticker/:address
GET /dogespells/spells/:txid
```

### DMP

DMP is the open inscription-based marketplace standard for Doginals. Every listing, bid, settlement, and cancel is an on-chain inscription with `"protocol":"DMP","version":"1.0"`. PSBTs live entirely off-chain (IPFS or Arweave CID) — never on-chain.

Enable in config (default `true` when section is absent):

```toml
[protocols.dmp]
enabled = true
```

Run a dedicated DMP-only sync:

```bash
kabosu doginals index sync --only dmp --config-path kabosu.toml
```

**Webhook events:** `dmp.listing`, `dmp.bid`, `dmp.settle`, `dmp.cancel`

**API:**
```text
GET /api/dmp/listings          # Active (non-cancelled, non-settled) listings
```

**Wire format (inscription body):**
```json
{
  "protocol": "DMP",
  "version": "1.0",
  "op": "listing | bid | settle | cancel",
  "listing_id": "<inscription_id of original listing>",
  "seller": "D... address",
  "price_koinu": 4206900000,
  "psbt_cid": "ipfs://Qm...",
  "expiry_height": 5000000,
  "nonce": 12345,
  "signature": "hexsig"
}
```

## Web Explorer

Kabosu includes a lightweight built-in explorer + JSON API server.

### Quick Start

1. Enable web explorer in `kabosu.toml`:

```toml
[web]
enabled = true
port = 8080
```

2. Start the indexer service:

```bash
kabosu doginals service start --config-path kabosu.toml
```

3. Open your browser to **http://localhost:8080**

### Available routes

- `GET /health`
- `GET /api/status`
- `GET /api/inscriptions`
- `GET /api/inscriptions/recent`
- `GET /api/drc20/tokens`
- `GET /api/dunes/tokens`
- `GET /api/dogelotto/tickets`
- `GET /api/dogelotto/winners`
- `GET /api/dns/names`
- `GET /api/dogemap/claims`
- `GET /api/dogetags`
- `GET /api/dmp/listings`
- `GET /dogespells/balance/:ticker/:address`
- `GET /dogespells/history/:ticker/:address`
- `GET /dogespells/spells/:txid`
- HTML pages: `/`, `/inscriptions`, `/drc20`, `/dunes`, `/lotto`

The bundled HTML currently focuses on lotto flows (tickets, QR, burn actions).

### Production Deployment

For production, run behind a reverse proxy (nginx/Cloudflare) pointing at `localhost:8080`.

Example nginx config:

```nginx
server {
   listen 80;
   server_name kabosu.yourdomain.com;
    
   location / {
      proxy_pass http://localhost:8080;
      proxy_set_header Host $host;
      proxy_set_header X-Real-IP $remote_addr;
   }
}
```

Or use Cloudflare Tunnel (see launcher scripts in `C:\Users\<user>\bin\kabosu-launch.bat`).

### API Endpoints

The web explorer also exposes JSON APIs:

- `GET /api/status` — Indexer status, total inscriptions, latest block
- `GET /api/inscriptions/recent` — Last 20 inscriptions
- `GET /api/inscriptions?limit=50&offset=0` — Paginated inscriptions
- `GET /api/drc20/tokens?limit=50` — DRC-20 tokens
- `GET /api/dunes/tokens?limit=50` — Dunes tokens
- `GET /api/dogelotto/tickets?limit=50` — DogeLotto tickets
- `GET /api/dogelotto/winners?limit=50` — DogeLotto winners
- `GET /api/dmp/listings?limit=50` — Active DMP listings
- `GET /health` — Health check

All APIs return JSON. Use these to build custom dashboards or integrations.

## Quick Start

**User privacy note:** All user-specific configuration (like usernames and paths) is handled via environment variables and local config files that are ignored by git. No private user info will be committed to the repository.

See [SETUP.md](SETUP.md) for a step-by-step setup guide.

1. Run a Dogecoin Core node with:

   ```bash
   -txindex=1 -zmqpubrawblock=tcp://127.0.0.1:28332 -zmqpubrawtx=tcp://127.0.0.1:28332
   ```

2. Set RPC credentials:

   ```bash
   export DOGECOIN_DATA_DIR=/path/to/dogecoin-data   # optional if Core uses the default data dir
   export DOGE_RPC_USERNAME=youruser
   export DOGE_RPC_PASSWORD=yourpass
   ```

   PowerShell:

   ```powershell
   $env:DOGECOIN_DATA_DIR="F:\DogecoinData"          # optional if Core uses the default data dir
   $env:DOGE_RPC_USERNAME="youruser"
   $env:DOGE_RPC_PASSWORD="yourpass"
   ```

   When `DOGECOIN_DATA_DIR` is unset, kabosu auto-detects Dogecoin Core's
   default data directory. The shadow blk-index copy now defaults to
   `<dogecoin-data-dir>/<network>/blk-index`, so it stays on the same drive as
   the node by default.

   Optional `.env` workflow (repo root):

   ```bash
   cp .env.example .env
   # edit .env with your real DOGE_RPC_USERNAME / DOGE_RPC_PASSWORD
   ```

3. Copy and edit the config:

   ```bash
   cp kabosu.toml my-indexer.toml
   # edit my-indexer.toml
   ```

4. Migrate the database:

   ```bash
   kabosu doginals database migrate --config-path my-indexer.toml
   ```

5. Start indexing:

   ```bash
   kabosu doginals service start --config-path my-indexer.toml
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
urls    = ["https://api.yourapp.com/hooks/kabosu"]
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

Dogecoin produces 1-minute blocks and experiences more frequent reorgs than Bitcoin. kabosu handles this automatically:

- ZMQ `rawblock` stream tracks the live chain tip
- On reorg detection: `rollback_block` removes inscription data, DNS registrations, Dogemap claims, and DRC-20 operations for each orphaned block — in a single Postgres transaction
- Replay continues from the fork point

## Webhook Payload Examples + Sample Receiver

kabosu fires real-time webhooks on every inscription event (reveals, transfers, DRC-20 ops, Dunes etching, etc.).
Webhooks are atomic and reorg-safe — if a block is reorged, kabosu rolls back and re-sends corrected events.

### Example payloads (JSON)

**Inscription Revealed**
```json
{
  "event": "inscription_revealed",
  "inscription_id": "abc123...i0",
  "tx_id": "0x...",
  "block_height": 4609723,
  "block_hash": "0x...",
  "content_type": "image/png",
  "content": "89504e470d0a1a0a...",
  "inscriber": "D...address",
  "parents": ["..."],
  "delegate": null,
  "metaprotocol": "drc-20",
  "timestamp": 1730000000
}
```

**Inscription Transferred**
```json
{
  "event": "inscription_transferred",
  "inscription_id": "abc123...i0",
  "from": "D...old",
  "to": "D...new",
  "block_height": 4609724
}
```

**DRC-20 Mint / Transfer** (same shape for Dunes, DNS, Dogemap, etc.)
```json
{
  "event": "drc20_mint",
  "tick": "DOGE",
  "amount": "1000",
  "to": "D...address",
  "block_height": 4609725
}
```

### Sample webhook receiver (Node.js / Express)

```javascript
// webhook-receiver.js
const express = require('express');
const app = express();
app.use(express.json());

app.post('/webhook', (req, res) => {
  const event = req.body;
  console.log(`[${new Date().toISOString()}] ${event.event} at block ${event.block_height}`);

  // Add your logic here (Discord, database, trading bot, etc.)
  if (event.event === 'inscription_revealed' && event.content_type.startsWith('image/')) {
    console.log('New image inscription! ID:', event.inscription_id);
  }

  res.sendStatus(200);
});

app.listen(3000, () => console.log('Webhook receiver running on port 3000'));
```

Add to `kabosu.toml`:
```toml
[webhooks]
urls = ["http://localhost:3000/webhook"]
```

---

## Deployment Guide

### 1. Docker (recommended for production)

```dockerfile
# Dockerfile
FROM rust:1.80 as builder
WORKDIR /app
COPY . .
RUN cargo build --release --package cli

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/kabosu /usr/local/bin/
COPY kabosu.toml /etc/kabosu.toml
CMD ["kabosu", "doginals", "index", "--config-path", "/etc/kabosu.toml"]
```

```bash
docker build -t kabosu .
docker run -d \
  -v /path/to/dogecoin-data:/root/.dogecoin \
  -v /path/to/kabosu.toml:/etc/kabosu.toml \
  -p 8080:8080 \
  kabosu
```

DogeSpells migrations run automatically on startup. Once the indexer is live, query:

```text
/dogespells/balance/<ticker>/<address>
/dogespells/history/<ticker>/<address>
/dogespells/spells/<txid>
```

### 2. systemd service (bare-metal)

```ini
# /etc/systemd/system/kabosu.service
[Unit]
Description=Kabosu Dogecoin Doginals Indexer
After=network.target

[Service]
ExecStart=/usr/local/bin/kabosu doginals index --config-path /etc/kabosu.toml
Restart=always
User=youruser
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl enable --now kabosu
```

### 3. Nginx reverse proxy

```nginx
server {
    listen 80;
    server_name kabosu.yourdomain.com;

    location / {
        proxy_pass http://localhost:8080;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
    }

    location /metrics {
        proxy_pass http://localhost:8080/metrics;
        allow 127.0.0.1;
        deny all;
    }
}
```

---

## API Endpoints Reference

kabosu ships with a lightweight explorer + REST API on the configured port (default `8080`).

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/` | Web explorer UI |
| GET | `/api/inscription/:id` | Single inscription details |
| GET | `/api/block/:height` | Block summary + inscriptions |
| GET | `/api/address/:addr` | All inscriptions owned by address |
| GET | `/api/search?content_type=image/png` | Search by MIME, tick, etc. |
| GET | `/metrics` | Prometheus metrics (file vs rpc counts) |
| POST | `/webhook/test` | Test webhook delivery |

All API responses are JSON and support `?limit=100&offset=200` pagination.

---

## Predicate Filter Cookbook

The `--predicate` flag (and config) lets you index exactly what you care about.

### Basic examples

```bash
--predicate "mime:image/"                    # Any image
--predicate "mime:text/plain"                # Plain text only
--predicate "content-prefix:abc123"          # Content starts with these bytes
--predicate "inscriber:D...specificaddress"  # Only one inscriber
```

### Advanced combinations (AND/OR)

```bash
--predicate "mime:image/ OR mime:video/"
--predicate "mime:text/plain AND content-prefix:hello"
--predicate "metaprotocol:drc-20 AND tick:DOGE"
```

### Full list of supported filters

- `mime:<prefix>` (alias: `content_type:`)
- `content-prefix:<hexbytes>`
- `inscriber:<address>`
- `metaprotocol:drc-20` / `dunes` / `dogemap` etc.
- `tick:<XXXX>` (for DRC-20 / Dunes)
- `parent:<inscription_id>`

> **Pro tip:** Test your predicate first with `scan` before enabling it on the full indexer!

---

## Complete Example kabosu.toml

```toml
# kabosu.toml — Full production example

[dogecoin]
data_dir = "C:\\Users\\jheav\\AppData\\Roaming\\Dogecoin"   # ← Change this
data_source = "auto"          # "auto" (recommended), "file", or "rpc"
stop_block = null             # Optional: cap any sync/scan

[server]
port = 8080
prometheus = true

[webhooks]
urls = [
    "http://localhost:3000/webhook",
    "https://your-discord-webhook.com"
]
# Optional: only send certain events
# events = ["inscription_revealed", "drc20_mint"]

[doginals]
# Optional: extra predicates for the full indexer
predicates = [
    "mime:image/",
    "mime:text/plain"
]

[dunes]
# Same predicates available for dunes
```

---

## Building

```bash
cargo build --release
```

The binary is at `target/release/kabosu`.

---

Made with love for the Doge community. Much index. Very fast. Wow.
