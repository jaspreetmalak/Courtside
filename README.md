# Courtside

Rust AI phone assistant for web and iOS. [Explore the illustrative demo](https://courtside-assistant.jass-malak12.chatgpt.site/demo/).

This is a development project, not a production-ready hosted calling service.
Real calls require your own configured backend, Twilio and OpenAI accounts,
and incur provider charges. The linked demo uses sample interactions.

An AI phone assistant: it makes and receives calls for you — navigating phone
trees and waiting on hold — while you watch the live transcript, listen in,
jump in, or take a transfer. Post-call, it writes you a summary.

## Architecture

```
 Phone ◄─PSTN─► Twilio ◄─Media Stream WS─► server (Axum) ◄─WS─► OpenAI Realtime
                                              │  SeaORM/SQLite
                                              └─REST + WS─► Dioxus app (Web + iOS)
```

- **`server/`** — Axum + Tokio. Bridges Twilio Media Streams (G.711 μ-law)
  to the OpenAI Realtime API, persists calls + transcripts (SeaORM/SQLite),
  broadcasts live events to clients, and summarizes finished calls.
- **`dioxus-client/`** — shared Dioxus browser and iOS app: assistant chat, calls, number claim,
  transcript, and new-call form. It has its own Cargo workspace, assets,
  signing, and device installer.
- **`shared/`** — DTOs and the client↔server wire protocol. No logic, no
  secrets. The only crate both sides depend on.

The client never links server code; the two workspaces communicate over HTTP.

## Web app

The browser build uses the existing Dioxus client and Rust service. `/` is the
landing page, `/app/` is the authenticated web app, and `/demo/` is an optional
guided example. Sign-in, assistant chat, call history, settings, live listening,
and microphone takeover run in the browser. New web accounts can plan calls
before verifying caller ID; real calls still require server-side verification.

```sh
rustup target add wasm32-unknown-unknown
./dioxus-client/scripts/build-web.sh
python3 -m http.server 8080 --bind 127.0.0.1 --directory web/dist
```

Set `COURTSIDE_SERVER_URL` at build time for a permanent HTTPS backend. Add the
website's exact origin to `WEB_ORIGINS` on that server. Hosting and browser
checks are documented in [web/README.md](web/README.md).

## Dev loop

Prereqs: Rust, the Dioxus CLI, Xcode with the iOS SDK, an ngrok (or similar)
tunnel, a Twilio account + number, and an OpenAI API key.

1. `cp .env.example .env` and fill in the keys.
2. Start the server and its reusable ngrok tunnel:
   ```bash
   ./scripts/run-local.sh
   ```
3. In another terminal, build, sign, install, and launch the app on the configured iPhone:
   ```bash
   ./dioxus-client/scripts/run-device.sh
   ```
4. Create an account and verify a real mobile number. Free users receive one
   lifetime 15-minute trial and place outbound calls through a temporary number
   from the shared pool. A free user never owns that number and cannot receive
   callbacks on it.
5. Courtside Pro is a `$19.99/month` StoreKit 2 subscription with 120 included
   minutes per App Store billing period. A paid user chooses one dedicated
   number, which supports inbound and outbound calls. Cancellation keeps access
   and the number through the paid-through date; the server releases it after
   entitlement expiry.
6. Configure App Store Server Notifications V2 to post to
   `https://PUBLIC_HOST/api/apple/notifications`. The server verifies StoreKit
   JWS certificate chains and treats its entitlement/usage ledger as
   authoritative.
7. For production, enable App Attest + DeviceCheck using the variables in
   `.env.example`. Leave `REQUIRE_APP_ATTEST` unset for simulator development.
8. In the app: **New Call** → a destination number + a task → watch the live
   transcript; the summary lands after hangup. Calls to a paid user's dedicated
   number are answered by the assistant automatically. Incoming calls get a
   separate chat with factual caller updates. When a caller asks for the owner,
   Courtside offers a 45-second in-app invitation to join the same call; if it
   is declined or unanswered, the assistant takes a message. Native incoming
   CallKit invitations currently require the app to be open. Reliable waking
   and ringing while closed needs PushKit/APNs setup and push-enabled signing.
9. Open **Call forwarding** in the app menu (or **Account → Set up call
   forwarding**) to copy your dedicated, inbound-enabled Courtside number and
   follow the iPhone all-calls or carrier missed-calls setup guide. Your carrier
   controls activation and cancellation; the app does not claim forwarding is
   enabled just because a guide was selected. Verify it by calling your usual
   mobile number from another phone and checking the incoming call in Courtside.

During either incoming or outgoing calls, Courtside asks the owner in private
chat when the other party needs an answer that is missing from the supplied
facts. The caller hears an initial wait limit of up to 20 seconds, then a brief
update at 10 seconds remaining. A question panel above the main chat composer,
outside the transcript card, has a dedicated answer field and
Send answer button with the same deadline, including after reconnecting or
switching chats. The field disappears after an accepted answer or expiry.
Replies are scoped to the owner, call,
and question and go directly into the existing GPT-Live conversation. If the
owner does not answer before the deadline, Courtside speaks an update and continues without that
detail where possible, or takes a message and wraps up; it never invents an
answer or keeps restarting the wait. A chat question does not ring CallKit.

Courtside can look back through the owner's actual previous chats for missing
context. GPT-6 Luna searches relevant conversation text, then reads surrounding
messages before answering. Search covers existing chats as well as new ones;
details do not need to have been saved separately. The app shows “Remembering…”
only during actual retrieval. Lookup is restricted to the authenticated owner,
returns bounded excerpts and pages, omits failed drafts, and never changes the
selected chat or grants permission to place a call. The full-text index follows
inserts, edits, and deletions of the original messages.

Account has a **Reference past chats** switch. There is no saved-facts store,
auto-saving tool, or memory editor. During a call, missing details trigger a
spoken request to wait and an asynchronous search of original chats. Luna can
return only an exact, verified excerpt of the owner's original words, relevant
to the authorized task; inbound callers need explicit permission in owner call
instructions before any non-sensitive detail can be supplied automatically.
Lookup starts with the caller's question and includes the original surrounding
messages, so a short owner reply such as a date can be understood in context.
The owner can reply privately while lookup runs. Lookup is capped at 8 seconds
and shares the same absolute 20-second deadline with the owner reply; errors and
missing evidence never extend it. Takeover, disconnect, or an accepted owner
reply cancels lookup. Chat and summaries use `gpt-6-luna`; live audio uses
`gpt-live-1`.

Calls use a delivery variation derived from their call ID. Reconnecting keeps
the same direction; a new call can use a different approach. The assistant
chooses wording from the goal and recipient's actual reply instead of reading
a fixed introduction. Explicit wording, language, anonymity, and announcement
preferences take precedence over variation.

The live decision backend receives the complete call objective and constraints
and uses low reasoning for quick tool decisions. Ordinary replies and choices
already within the owner's limits stay in the voice model. It accepts authorized alternatives,
asks privately before going outside those limits, and clarifies ambiguous
statements instead of inventing dates or outcomes. Preflight checks essential
facts before calling while allowing necessary private questions about later
contingencies during the connected call. Preflight planning uses medium reasoning
before the phone rings. The live audio clock keeps a fixed 20ms cadence, and
replay removes the quiet tail left by connecting a new voice session. A bounded
startup burst primes the actual reply without permanently buffering live input.
Digital-silence output packets are omitted so they cannot queue ahead of speech
on the phone. Logs track reply gaps, input buffering, and backend durations.

## Number and minute policy

Courtside uses a platform-owned Twilio account: end users do not connect or
fund their own Twilio accounts. The server enforces one active call per account,
passes the exact remaining allowance to Twilio as `TimeLimit`, and finalizes
outbound usage from Twilio's completed callback. The free pool targets one
number, caps itself at three, and can buy at most one replacement per day.
Numbers are retained through their prepaid month and only released near renewal;
paid numbers are never recycled into the free pool.

Tests: `cargo test`. Starting the real server requires the environment variables
in `.env.example`. Unit tests use test configuration and mock services.

## Current limitations

- The backend needs a persistent SQLite database and a stable public HTTPS/WSS endpoint.
- Incoming iOS invitations currently require the app to be open; PushKit/APNs is not configured.
- Production credential setup, phone verification, usage enforcement, billing and privacy requirements must be reviewed before operating a public service.
- iOS device builds require your own provisioning profile, `DEVICE_ID` and `SIGN_ID` environment variables.

## Third-party code

`dioxus-client/vendor/dioxus-desktop` contains a patched Dioxus component. Its
upstream MIT and Apache-2.0 notices are preserved in that directory. No license
has been selected for Courtside's original code.
