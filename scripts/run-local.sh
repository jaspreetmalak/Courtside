#!/usr/bin/env bash
# Start/reuse an ngrok tunnel, discover its rotating HTTPS hostname, and run
# the Courtside server with the correct PUBLIC_HOST for Twilio webhooks.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NGROK_API="http://127.0.0.1:4040/api/tunnels"
NGROK_PID=""

cleanup() {
    if [ -n "$NGROK_PID" ]; then
        kill "$NGROK_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT INT TERM

mkdir -p "$ROOT/target"

if ! curl -fsS --max-time 1 "$NGROK_API" >/dev/null 2>&1; then
    echo "▶ Starting ngrok"
    ngrok http 3000 --log=stdout --log-format=json > "$ROOT/target/ngrok.log" 2>&1 &
    NGROK_PID=$!
fi

PUBLIC_URL=""
for _ in {1..40}; do
    PUBLIC_URL=$(curl -fsS --max-time 1 "$NGROK_API" 2>/dev/null \
        | jq -r '[.tunnels[] | select(.proto == "https")][0].public_url // empty' \
        || true)
    [ -n "$PUBLIC_URL" ] && break
    sleep 0.25
done

if [ -z "$PUBLIC_URL" ]; then
    echo "Could not discover an HTTPS ngrok tunnel. Check $ROOT/target/ngrok.log." >&2
    exit 1
fi

export PUBLIC_HOST="${PUBLIC_URL#https://}"
echo "▶ Twilio public host: $PUBLIC_HOST"
echo "▶ Starting Courtside on http://0.0.0.0:3000"

cd "$ROOT"
cargo run -p server
