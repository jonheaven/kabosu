# kabosu — Dogecoin Indexer

[![Crates.io](https://img.shields.io/crates/v/doge-lotto)](https://crates.io/crates/doge-lotto)
[![Docs](https://img.shields.io/docsrs/doge-lotto)](https://docs.rs/doge-lotto)
[![MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![DogeLotto](https://img.shields.io/badge/DogeLotto-official_standard-blue)](https://github.com/jonheaven/doge-lotto)

The fastest, lightest, most selective Doginals indexer for Dogecoin.
Backward-traversal + reorg-safe (Chainhook engine) + Hiro-style predicate filtering + real-time webhooks.

## dog vs kabosu

| Selective indexing | No | Yes — MIME type + content prefix predicates |
| DNS / Dogemap / DogeLotto | Query only | Indexed natively, queryable via CLI + webhooks |
| Real-time hooks | No | Yes — POST JSON on every DNS/Dogemap event |
| Use when | You want a local explorer or wallet | You're building an app, API, or analytics pipeline |

Both projects are completely independent codebases. kabosu does not import dog.

## Supported Protocols

| Protocol | Status | CLI commands |
| --- | --- | --- |  
| Doginals (inscriptions) | Full — backward traversal, reorg-safe | `kabosu doginals service start` |
| DRC-20 | Full | via `doginals` indexer |
| Dunes | Full | `kabosu dunes service start` |
| DNS (Dogecoin Name System) | Full — 28 namespaces, first-wins, reorg-safe | `kabosu dns resolve`, `kabosu dns list` |
| Dogemap (block claims) | Full — first-wins, reorg-safe | `kabosu dogemap status`, `kabosu dogemap list` |
| DogeLotto | Full — deploys, atomic ticket mints, auto-resolution, Burners mechanic | `kabosu lotto deploy`, `kabosu lotto mint`, `kabosu lotto list`, `kabosu lotto status`, `kabosu lotto burn`, `kabosu lotto burners`, `kabosu doginals index sync --only dogelotto` |

### Metaprotocols

**Official standard:** [github.com/jonheaven/doge-lotto](https://github.com/jonheaven/doge-lotto)
| Dogetag (on-chain graffiti) | Full — OP_RETURN text messages, reorg-safe | `kabosu dogetag list`, `kabosu dogetag search`, `kabosu dogetag address`, `kabosu dogetag send` |
| DogeSpells | Full — OP_RETURN magic-prefix + CBOR spells, balances, NFT metadata snapshots, reorg-safe | `kabosu doginals index sync --only dogespells` plus `/dogespells/*` API routes |
| DMP | Full — inscription-based marketplace: listings, bids, settlements, cancels; reorg-safe | `kabosu doginals index sync --only dmp`, `GET /api/dmp/listings` |

### DogeLotto

**Official DogeLotto Meta-Protocol Standard** → [jonheaven/doge-lotto](https://github.com/jonheaven/doge-lotto)

`kabosu lotto mint` broadcasts one atomic transaction that:

- Pays the deploy's `prize_pool_address` the exact `ticket_price_koinu` amount.
- Optionally pays an immutable protocol developer tip in the same transaction.
- Inscribes the `DogeLotto` mint JSON (with `"p":"DogeLotto"`) in the same transaction.

This lets the indexer verify payment and tip commitments trustlessly.

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

---

## For all other protocols, see the full documentation and CLI help

---

Made with love for the Doge community. Much index. Very fast. Wow.
