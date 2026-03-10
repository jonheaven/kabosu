#!/bin/bash
set -e

echo "=== Doghook Docker Entrypoint ==="

echo "Running database migrations..."
doghook doginals database migrate --config-path /etc/doghook.toml

echo "Starting doghook service..."
exec doghook doginals service start --config-path /etc/doghook.toml
