#!/usr/bin/env bash
# Package the shared Dioxus client and landing page for static web hosting.
set -euo pipefail
CLIENT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WEB_ROOT="$(cd "$CLIENT_ROOT/../web" && pwd)"
if [ -z "${COURTSIDE_SERVER_URL:-}" ]; then
    COURTSIDE_SERVER_URL="$(curl -fsS --max-time 2 http://127.0.0.1:4040/api/tunnels \
        | python3 -c 'import json,sys; print(next((x["public_url"] for x in json.load(sys.stdin)["tunnels"] if x["proto"]=="https"), ""))' || true)"
fi
export COURTSIDE_SERVER_URL="${COURTSIDE_SERVER_URL:-http://127.0.0.1:3000}"
(
    cd "$CLIENT_ROOT"
    # Avoid shipping old hashed bundles left by previous Dioxus builds.
    rm -rf target/dx/courtside-mobile/release/web/public "$WEB_ROOT/dist"
    dx bundle --web --release --no-default-features --features web --out-dir "$WEB_ROOT/dist"
)
python3 "$WEB_ROOT/scripts/prepare-site.py"
