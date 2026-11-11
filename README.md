# Courtside

An AI phone assistant for web and iOS, built in Rust. Tell Courtside who to call and what you need done, then follow the conversation without sitting through phone menus or hold music.

Courtside brings call planning, live transcripts, private questions, and post-call summaries into one chat. You can listen in, join the conversation, or let the assistant continue.

[Explore the interactive demo](https://courtside-assistant.jass-malak12.chatgpt.site/demo/) · [Web setup](web/README.md) · [iOS client](dioxus-client/README.md)

## What the app does

- **Plan and place calls from chat.** Describe the task and its constraints, such as checking availability, arranging an appointment, or following up with customer support. The assistant checks for missing essential details before dialing.
- **Handle phone menus and hold time.** The voice assistant has tools for keypad selections, conversation updates, and ending the call.
- **Follow the conversation live.** See streaming transcripts and call status, listen to the audio, and join through the app's microphone. Call controls also support phone takeover and returning the conversation to the assistant.
- **Answer privately during a call.** When the assistant needs a detail you haven't supplied, a question appears in chat with a dedicated answer field and a deadline.
- **Receive calls on a dedicated number.** Incoming calls get their own chat with caller updates. When a caller asks for you, the assistant can invite you to join or take a message if you're unavailable.
- **Review what happened.** Call history keeps transcripts and generated summaries alongside your conversations.
- **Reference past chats.** An optional account setting lets the assistant search your original conversations for relevant context, rather than relying on a separate saved-facts store.
- **Choose how calls sound.** Set the assistant's voice, introduction preferences, and custom call instructions.

The interface is shared across the browser and iOS. Native integrations cover audio, CallKit, contacts, and StoreKit; the browser has its own listening and microphone support.

## A typical call

1. Give Courtside a destination number, a goal, and any limits or preferences.
2. Resolve any essential questions before the call starts.
3. Follow the live transcript while the assistant talks. Reply privately if it needs more information, or join the call yourself.
4. Read the summary when the call ends and revisit the transcript from call history.

For incoming calls, Courtside reports who is calling and why, then offers an invitation when the caller wants to speak with you. The app also includes a forwarding guide for sending calls from your usual number to a dedicated Courtside number; activation remains with your carrier.

## How it works

```text
Phone <-> Twilio <-> Rust server <-> OpenAI Realtime
                         |
                    REST + WebSocket
                         |
                  Dioxus web / iOS app
```

- **[`server/`](server/)**: Axum and Tokio service that bridges Twilio Media Streams to OpenAI Realtime, manages call controls and private questions, and publishes live events. SeaORM and SQLite store accounts, chats, calls, transcripts, and usage.
- **[`dioxus-client/`](dioxus-client/)**: shared Rust/Dioxus interface with platform-specific audio and native integrations. It has its own Cargo workspace and build scripts.
- **[`shared/`](shared/)**: request/response types and the WebSocket event protocol used by both sides.
- **[`web/`](web/)**: web distribution scripts and browser checks. The landing page lives at `/`, the authenticated app at `/app/`, and the illustrative demo at `/demo/`.

The client communicates with the server over HTTP and WebSockets; provider credentials stay on the server. Live audio uses the Realtime API, with separate model configuration for chat, call planning, and summaries.

## Run locally

### Backend

You'll need Rust, configured Twilio and OpenAI accounts, and a public HTTPS/WSS endpoint for Twilio. The local helper uses ngrok, `curl`, and `jq`.

```sh
cp .env.example .env
# Fill in the provider credentials and PHONE_HASH_SECRET.
./scripts/run-local.sh
```

See [`.env.example`](.env.example) for database, model, browser-origin, and Apple integration settings. Keep credentials and runtime databases out of source control.

### Browser

Install the Dioxus CLI and the WebAssembly target, then build and preview:

```sh
rustup target add wasm32-unknown-unknown
./dioxus-client/scripts/build-web.sh
python3 -m http.server 8080 --bind 127.0.0.1 --directory web/dist
```

For a permanent backend, set `COURTSIDE_SERVER_URL` at build time and add the site's exact origin to the server's `WEB_ORIGINS`. See [web/README.md](web/README.md) for hosting and browser checks.

### iOS

Use macOS with Xcode, the iOS SDK, the Dioxus CLI, and a development provisioning profile. Set `DEVICE_ID` and `SIGN_ID` for your device and signing identity, then run:

```sh
./dioxus-client/scripts/run-device.sh
```

See [dioxus-client/README.md](dioxus-client/README.md) for client build details.

### Tests

Run the server/shared workspace tests from the repository root:

```sh
cargo test
```

Browser and audio checks are documented in [web/README.md](web/README.md). Live provider calls and physical-device audio require separate integration testing.

## Development notes

- The linked demo uses sample interactions and does not place calls. Running the real app requires a configured backend and incurs provider charges.
- Outbound calls currently support valid US/Canada `+1` numbers and require verified caller ID. The backend enforces account entitlements, minute allowances, and one active call per account.
- The code includes free-trial usage tracking and StoreKit subscription support with dedicated inbound numbers. Apple billing, notifications, and device attestation require your own configuration.
- Incoming iOS invitations currently require the app to be open. Reliable background ringing needs PushKit/APNs setup and push-enabled signing.
- A hosted service needs persistent SQLite storage, a stable public endpoint, and a review of authentication, billing, privacy, and calling requirements before public operation.

## Third-party code

`dioxus-client/vendor/dioxus-desktop` contains a patched Dioxus component with its upstream MIT and Apache-2.0 notices preserved. No license has been selected for Courtside's original code.
