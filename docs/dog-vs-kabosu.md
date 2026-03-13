# Kabosu vs Dog – Why We Keep Both Tools

**Updated March 2026**

You might be wondering:
**"We just made kabosu insanely fast and powerful… so why are we still keeping `dog` around?"**

Short answer: **They are deliberately different tools for different people.**
Keeping both makes the Dogecoin ecosystem *stronger*, not watered down.

---

## The Honest Comparison (2026 Reality)

| Use Case | **dog** (lightweight redb) | **kabosu** (production Postgres) | Winner |
|----------|---------------------------|-----------------------------------|--------|
| Quick local explorer + wallet | Perfect (tiny, zero-config, built-in wallet) | Overkill (needs Postgres + server) | **dog** |
| Full historical sync speed | Extremely fast (direct .blk files) | Equally fast + parallel 8-thread + hybrid mode | Tie |
| Selective indexing (images, DRC-20, etc.) | Has to index everything first | Filters **before** writing (Hiro predicates) | **kabosu** |
| Real-time webhooks & live tail | None | Yes (ZMQ + atomic reorg safety) | **kabosu** |
| API + explorer + Prometheus metrics | Basic | Full REST API + web UI + metrics | **kabosu** |
| `scan` command (export/inspect) | Basic list | Rich JSONL + full predicates + content | **kabosu** |
| Production / 24-7 service | Not designed for it | Built for it (Docker, systemd, scaling) | **kabosu** |
| Resource usage (idle) | Tiny | Slightly higher (Postgres) | **dog** |
| Crash safety & resume | Manual redb backup | Automatic per-block Postgres commits | **kabosu** |

---

## Why We Keep Both (And Why It's Not Watering Down the Ecosystem)

We could have deleted `dog` and said "just use kabosu + Postgres forever."
That would have been **rude** to everyone who loves the simple redb workflow.

Instead, we made a conscious decision:

1. **Different audiences need different tools**
   - Some people just want a fast local CLI explorer + wallet on their laptop with zero dependencies. That's `dog`.
   - Most people building websites, bots, analytics, or services need real-time webhooks, a scalable database, and an API. That's `kabosu`.

2. **They complement each other perfectly**
   - Use `dog` for quick testing, wallet stuff, or when you're offline.
   - Use `kabosu` for everything that needs to run 24/7 or serve data to users.
   Many power users (including the author) run **both** side-by-side.

3. **`dog` is not deprecated**
   It is intentionally maintained as the **lightweight companion tool**.
   `kabosu` is the **production powerhouse**.
   Both are actively improved and will stay that way.

4. **No forced migration**
   If you love `dog` exactly as it is, keep using it forever.
   If you want modern features (webhooks, API, fast selective scan, metrics), switch to kabosu. No pressure.

**Real talk**: For production and serious use, **kabosu is clearly superior** now (faster selective indexing, webhooks, deployment story, etc.).
But "superior" depends on what you need. Forcing everyone into Postgres would have been a worse experience.

---

## Recommended Setup for Most People

```bash
# Keep both repos side-by-side
C:/Users/<USER>/Desktop/doge/dog          # lightweight CLI companion + wallet
C:/Users/<USER>/Desktop/doge/kabosu      # main production indexer

# Daily workflow
kabosu doginals index                   # runs forever with webhooks
kabosu doginals index scan ...          # for testing/exporting
dog scan ...                             # only when you need the wallet
```

## Migration Path (If You Want to Switch)

1. Set `DOGECOIN_DATA_DIR` (or `dogecoin_data_dir` in `kabosu.toml`)
2. Run `kabosu doginals index refresh-blk-index`
3. Run `kabosu doginals index` once — it catches up using the same fast `.blk` tech

You instantly gain webhooks, API, powerful scan, and production features.
You don't have to delete `dog` — just start using kabosu for the heavy lifting.

---

## Bottom Line from the Maintainer

We keep `dog` because it's awesome for what it is — a beautiful, tiny, wallet-first Dogecoin tool.
We built `kabosu` because the ecosystem needed a real production indexer with speed, scalability, and modern features.
Together they give the Dogecoin community the best of both worlds instead of forcing a one-size-fits-all compromise.

- Most people should use **kabosu**.
- Some people should keep using **dog**.
- Both are here to stay.

---

Questions? Open an issue or ping me on X ([@jontype](https://x.com/jontype)).
Happy indexing!

