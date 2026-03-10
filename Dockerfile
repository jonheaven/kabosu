# ─── Build stage ──────────────────────────────────────────────────────────────
FROM rust:1.85 AS builder
WORKDIR /app
COPY . .
RUN cargo build --release --bin doghook

# ─── Runtime stage ────────────────────────────────────────────────────────────
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/doghook /usr/local/bin/doghook
COPY doghook.toml /etc/doghook.toml
COPY subsidies.json starting_koinu.json /etc/doghook/
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
