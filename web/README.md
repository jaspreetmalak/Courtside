# Courtside web distribution

The website and browser app are implemented in the existing Dioxus workspace,
including its shared authentication, chat, account settings, call history,
transcripts, live listening, and browser microphone participation.

- `/` — Dioxus landing page with interactive product examples.
- `/app/` — the real Dioxus application, backed by the existing Rust/Axum service.
- `/demo/` — a guided, clearly labeled sample conversation. It never calls the API
  or places a phone call.

Build from the repository root:

```sh
./dioxus-client/scripts/build-web.sh
```

The builder discovers the current HTTPS tunnel. Set `COURTSIDE_SERVER_URL` to a
permanent backend origin when deploying the Rust service elsewhere. The backend
requires `WEB_ORIGINS` to include this site's exact origin. Real calling uses the
same verification and minute policies as the native client.

Preview the resulting static bundle:

```sh
python3 -m http.server 8080 --bind 127.0.0.1 --directory web/dist
```

The generated `dist/` directory can be served by a static host. The landing
page and illustrative demo do not require a backend. Real app features require
the separately configured Rust service. No credentials or runtime database are
included in this source distribution.

Browser validation uses Playwright and Chrome. Install Playwright in your test
environment, or set `PLAYWRIGHT_MODULE` to its module path, and optionally set
`CHROME_EXECUTABLE_PATH` to Chrome. With the preview on port 8080, run these from
`web/`:

```sh
node tests/browser.mjs
node --test tests/audio.test.mjs
node tests/browser-audio.mjs
```

The live-backend check additionally creates an isolated QA account, exercises a
real assistant reply, and removes its own test records. It never requests a
phone call. Set `COURTSIDE_QA_DATABASE` to the matching development database and
run `node tests/live-backend.mjs` against that development backend only. Browser
audio tests use a simulated microphone and offline media fixture; physical
phone-call audio still needs a live device check.
