# ─── Build stage ──────────────────────────────────────────────────────────────
FROM rust:1.85 AS builder
WORKDIR /app
COPY . .
RUN cargo build --release --bin kabosu

# ─── Runtime stage ────────────────────────────────────────────────────────────
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/* \
    && groupadd -r kabosu && useradd -r -g kabosu -m kabosu

COPY --from=builder /app/target/release/kabosu /usr/local/bin/kabosu
COPY kabosu.toml /etc/kabosu.toml
COPY subsidies.json starting_koinu.json /etc/kabosu/
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh \
    && chown -R kabosu:kabosu /etc/kabosu /etc/kabosu.toml

USER kabosu

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]

