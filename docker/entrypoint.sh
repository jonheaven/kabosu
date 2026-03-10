#!/bin/bash
set -e

echo "=== Kabosu Docker Entrypoint ==="

echo "Running database migrations..."
kabosu doginals database migrate --config-path /etc/kabosu.toml

echo "Starting kabosu service..."

# Forward SIGTERM/SIGINT to the Rust process so tokio can drain gracefully.
_term() {
    echo "[entrypoint] Received SIGTERM — forwarding to kabosu (pid $child)..."
    kill -TERM "$child" 2>/dev/null
    wait "$child"
}
trap _term SIGTERM SIGINT

kabosu doginals service start --config-path /etc/kabosu.toml &
child=$!
wait "$child"

