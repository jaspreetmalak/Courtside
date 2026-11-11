# Courtside Dioxus client

The iOS client for Courtside's GPT-Live phone assistant.

From the repository root, start the development server with:

```bash
./scripts/run-local.sh
```

Build and install the device client with:

```bash
./dioxus-client/scripts/run-device.sh
```

For browser development from this directory:

```bash
dx serve
```

Run the keyboard viewport and multiline composer regression checks with:

```bash
node --test tests/*.test.cjs
```

Live call controls also refresh their state from the server every two seconds
to recover missed events after iOS suspends the app. Take over joins the existing
call through a native outgoing CallKit action on iPhone. Tapping Take over
starts the join directly; microphone permission is requested if needed.
iOS controls presentation of its ongoing call interface, which can be accessed
from the system call indicator. Courtside does not add a custom call screen.
CallKit handles mute, speaker/audio routes, activation, and local hang-up.
Microphone permission is required to join. Audio is relayed over a bearer-
authenticated 8 kHz G.711 mu-law socket without dialing a second number.
The microphone graph is built only after CallKit activates audio, and the AI
remains on the existing Twilio stream until native audio is ready.
Return to AI, local CallKit hang-up, or loss of the voice connection restores the
assistant on the same Twilio stream with the takeover transcript. End call in
the chat controls ends the whole call. Native participation survives navigation
and screen locking with the audio and VoIP background modes.

Calls to a paid account's dedicated Courtside number are screened on the
server. Carrier forwarding can route calls from an existing mobile number to
that dedicated number; Courtside cannot intercept iPhone cellular audio.
Each incoming call gets a separate chat with the caller's name, reason, and
meaningful live updates. An explicit request to speak with the owner creates
one 45-second invitation. iPhone presents it through native incoming CallKit;
the web app shows Join and Take a message controls. The assistant retains the
call until the owner accepts and the native/browser audio is ready. Declining
or missing the invitation asks Courtside to take a message instead. Returning
to AI retains the incoming screening tools and the same conversation.

This development build receives incoming invitations while the app is open.
Server screening and saved messages work with the app closed, but waking or
ringing a suspended app requires PushKit/APNs and a push-enabled provisioning
profile. The free Personal Team profile does not support push notifications;
background modes alone do not deliver incoming invitations. PushKit/APNs
registration and delivery are not configured in this build. See Apple's
[VoIP push delivery](https://developer.apple.com/documentation/pushkit/responding-to-voip-notifications-from-pushkit)
and [supported capabilities](https://developer.apple.com/help/account/reference/supported-capabilities-ios).

Missing owner facts or decisions become private chat questions with a 20-second
countdown. A separate question panel at the bottom of chat, above the main
composer, shows the question, timer, answer field, and Send answer button;
a question in another chat also appears in the invitation banner.
The answer field disappears when submitted or expired. The caller hears the wait limit and
an update at 10 seconds remaining. Sending a reply binds it to that call and
question, so navigation, expired waits, or a reply from a different account cannot
redirect it into another conversation. Normal chat messages are separate from
these scoped replies. An unanswered question expires with a spoken update, and the
assistant continues without the missing detail or offers to take a message.
This flow does not invoke CallKit or automatically request microphone access.

Account's **Reference past chats** switch controls read-only lookup through
original conversations. Actual search and reading show “Remembering…”, matched
to the displayed request and chat. There is no saved-facts feature or editor.
Live calls show the same status while looking for one missing factual detail.
The caller hears a brief request to wait. Lookup and a private owner answer
share one 20-second deadline; lookup alone is capped at 8 seconds. A direct
owner answer takes priority and cancels the lookup.

Prepare a signed device build without installing it with
`COURTSIDE_BUILD_ONLY=1 ./scripts/run-device.sh` from this directory.

Live listen uses a native playback-only audio session, so the Ring/Silent switch
does not mute it and no microphone permission is requested. Its authenticated
`/api/calls/{id}/listen` socket labels the caller, assistant, and user audio tracks
with binary prefix bytes 1, 2, and 3 respectively (followed by G.711 mu-law).
Each track has an independent player, allowing both sides to be heard together
without inbound silence accumulating behind assistant speech. A prefix without
audio clears that track. Real-time caller buffering is limited to 300 ms;
assistant bursts retain their utterances. Connection errors reset the control
instead of leaving the button showing “Listening”.

The native Swift bridge is compiled by `build.rs` for iOS only. Device packaging
adds the microphone permission description and validates the audio and VoIP background modes. The older
phone-dial takeover endpoint remains available for previously installed clients.

The host Swift codec and offline audio-engine checks can be run without a
simulator or an audible phone call:

```bash
swiftc ios/VoiceCodec.swift tests/VoiceCodecTests.swift -o target/voice-codec-tests
target/voice-codec-tests
swiftc ios/VoiceCodec.swift ios/ListenAudio.swift tests/ListenAudioTests.swift -o target/listen-audio-tests
target/listen-audio-tests
```
