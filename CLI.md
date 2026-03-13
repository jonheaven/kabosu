# Full CLI Reference

All commands live under `kabosu doginals` or `kabosu dunes`.
The two namespaces are identical in syntax and flags — just swap `doginals` ↔ `dunes`.

## Global Options (work with every command)

| Flag | Description | Example |
|------|-------------|---------|
| `--config-path <PATH>` | Path to `kabosu.toml` (default: `./kabosu.toml` or `~/.config/kabosu.toml`) | `--config-path /etc/kabosu.toml` |
| `--data-dir <PATH>` | Override Dogecoin Core data directory (overrides config file) | `--data-dir %USERPROFILE%/Desktop/` |

---

## 1. `kabosu doginals index` — Full Production Sync

**Purpose:** Runs the complete indexer (historical catch-up + live ZMQ tail). Stores everything in Postgres, fires webhooks, and serves the explorer/API.

```bash
# Normal full sync (recommended for production)
kabosu doginals index

# Sync only a specific range (perfect for catching up or backfilling)
kabosu doginals index --from 4600000 --to 4700000

# Quick dev test (10 000 blocks in seconds — uses .blk files)
kabosu doginals index --test-blk-range 5000000..5000100
```

### Key flags

- `--from <HEIGHT>` — Start indexing from this block (inclusive)
- `--index-rare-koinu` — Force start at height `0` (includes pre-Doginals rare-koinu era)
- `--to <HEIGHT>` — Stop indexing at this block (inclusive). After this block the indexer stops (no live tail).
- `--test-blk-range <START>..<END>` — Shorthand for `--from START --to END --data-source file` (forces fast mode, great for testing).

### Use cases

- **Daily production run:** just `kabosu doginals index` (runs forever)
- **Backfill a specific week:** `--from 4650000 --to 4660000`
- **Include rare-koinu era:** `--index-rare-koinu`
- **Test a new predicate before production:** `--test-blk-range 5000000..5001000`

---

## 2. `kabosu doginals index scan` — Lightning-Fast Inspector / Exporter

**Purpose:** The killer command. Parses any range of blocks with **zero database writes**. Perfect for debugging, exporting, or analytics.

```bash
# Quick look at a range
kabosu doginals index scan --from 4609723 --to 4609823

# Export to file + filter only image reveals
kabosu doginals index scan \
  --from 4600000 \
  --to 4700000 \
  --out images.jsonl \
  --predicate "mime:image/" \
  --reveals-only

# Pipe to jq for instant stats
kabosu doginals index scan --from 5000000 --to 5001000 --reveals-only | jq -r '.content_type' | sort | uniq -c
```

### Key flags

- `--from <HEIGHT>` / `--to <HEIGHT>` — Required range
- `--out <PATH>` — Write JSONL to file (default: stdout)
- `--reveals-only` — Skip transfer events (only shows new inscriptions)
- `--content-type <PREFIX>` — Filter by content-type prefix (e.g. `image/`, `text/plain`)
- `--predicate <FILTER>` — Shorthand filter syntax:
  - `"mime:image/"` — filter by content-type prefix
  - `"mime:text/plain"` — exact content-type match prefix
  - Overrides `--content-type` when both are given

### Output format (one JSON object per line)

```json
{
  "event": "inscription_revealed",
  "inscription_id": "abc123...i0",
  "tx_id": "0x...",
  "block_height": 4609723,
  "content_type": "image/png",
  "content": "89504e47...",
  "inscriber": "D...address",
  "parents": [],
  "delegate": null
}
```

### Use cases

- "Why didn't this inscription appear?" → scan the block
- Export 1 million images for a website
- Daily analytics script
- Test predicates before enabling them in production

---

## 3. `kabosu doginals index refresh-blk-index`

**Purpose:** Refreshes the safe shadow copy of Dogecoin Core's LevelDB block index so the fast `.blk` reader works.

```bash
kabosu doginals index refresh-blk-index
```

Set `DOGECOIN_DATA_DIR` (or `dogecoin.dogecoin_data_dir` in config) when Core
does not use the platform default data directory. The shadow copy defaults to
`<dogecoin-data-dir>/<network>/blk-index`.

**Output example:**
```
BlkReader: index copy refreshed (3 updated, 1247 unchanged) → C:/Users/<USER>/Desktop/
Run 'kabosu doginals index' to enjoy 5-20× faster sync!
```

**When to run:**

- First time after starting Core
- After Core has synced new blocks (daily cron is perfect)

---

## Same Commands for Dunes

Just replace `doginals` with `dunes`:

```bash
kabosu dunes index --from 4600000 --to 4700000
kabosu dunes index scan --from 4609723 --to 4609823 --out dunes.jsonl
kabosu dunes index refresh-blk-index
```

---

## Pro Tips & Common Patterns

```bash
# 1. Daily dev workflow
kabosu doginals index scan --test-blk-range 5000000..5001000 --predicate "mime:image/" --out today.jsonl

# 2. Full production with fast mode guaranteed
kabosu doginals index --data-source file

# 3. Export everything from genesis to now (takes hours, not days)
kabosu doginals index scan --from 0 --to 5000000 --out all-reveals.jsonl --reveals-only

# 4. See real-time speed metrics
curl http://localhost:8080/metrics | grep blocks_indexed_via
```

> **Pro tip:** Set `data_source = "auto"` in `kabosu.toml` once and forget it — kabosu will always use the fastest possible method and tell you in the logs.

