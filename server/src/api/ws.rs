//! Authenticated client event stream. Events are filtered by call ownership so
//! each app instance only receives its user's calls.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use shared::{ClientEvent, ServerEvent};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use super::auth::AuthedUser;
use crate::bridge::voice::{VoiceCommand, VoiceOutput};
use crate::state::AppState;

/// Full-duplex native voice. Authorization uses the app's bearer header, and
/// only the owner of a connected call can attach a microphone to its bridge.
pub async fn call_voice_handler(
    Path(call_id): Path<Uuid>,
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    user: AuthedUser,
) -> Response {
    match crate::db::find_call_for_user(&state.db, call_id, user.0.id).await {
        Ok(Some(call)) if call.status == "in_progress" => {}
        Ok(Some(_)) => {
            return (
                axum::http::StatusCode::CONFLICT,
                "The call is not connected.",
            )
                .into_response()
        }
        _ => return axum::http::StatusCode::NOT_FOUND.into_response(),
    }
    ws.max_message_size(4096)
        .on_upgrade(move |socket| voice_session(socket, state, call_id))
}

async fn voice_error(socket: &mut WebSocket, message: &str) {
    let _ = socket
        .send(Message::text(
            serde_json::json!({"type":"error", "message":message}).to_string(),
        ))
        .await;
}

async fn voice_session(mut socket: WebSocket, state: AppState, call_id: Uuid) {
    // CallKit must have activated native audio before we silence the AI.
    let ready = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(Ok(message)) = socket.recv().await {
            match message {
                Message::Text(text)
                    if serde_json::from_str::<serde_json::Value>(&text)
                        .ok()
                        .is_some_and(|event| event["type"] == "ready") =>
                {
                    return true
                }
                Message::Close(_) => return false,
                _ => {}
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    if !ready {
        return;
    }
    let Some(relay) = state.voice_relay(call_id).await else {
        voice_error(
            &mut socket,
            "Call audio is still connecting. Try again in a moment.",
        )
        .await;
        return;
    };
    let session = Uuid::new_v4();
    let (output, mut frames) = mpsc::channel(25);
    let (reply, joined) = oneshot::channel();
    if relay
        .send(VoiceCommand::Join {
            session,
            output,
            reply,
        })
        .await
        .is_err()
    {
        voice_error(&mut socket, "That call has already ended.").await;
        return;
    }
    let result = tokio::time::timeout(Duration::from_secs(15), joined).await;
    match result {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(error))) => {
            voice_error(&mut socket, &error).await;
            return;
        }
        _ => {
            voice_error(
                &mut socket,
                "Unable to join call audio. Courtside will keep the call.",
            )
            .await;
            let _ = relay
                .send(VoiceCommand::Leave {
                    session: Some(session),
                    reply: None,
                })
                .await;
            return;
        }
    }
    if socket
        .send(Message::text(r#"{"type":"connected"}"#))
        .await
        .is_ok()
    {
        let idle = tokio::time::sleep(Duration::from_secs(10));
        tokio::pin!(idle);
        loop {
            tokio::select! {
                frame = frames.recv() => {
                    match frame {
                        Some(VoiceOutput::Audio(audio)) => {
                            if tokio::time::timeout(Duration::from_secs(2), socket.send(Message::Binary(audio.into())))
                                .await.map_or(true, |result| result.is_err()) { break; }
                        },
                        ended => {
                            let message = match ended {
                                Some(VoiceOutput::Ended(message)) => message,
                                _ => "Returning the call to Courtside…".into(),
                            };
                            let _ = socket.send(Message::text(serde_json::json!({"type":"ended", "message":message}).to_string())).await;
                            break;
                        }
                    }
                }
                message = socket.recv() => match message {
                    Some(Ok(Message::Binary(bytes))) if !bytes.is_empty() && bytes.len() <= 1600 => {
                        idle.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(10));
                        if relay.send(VoiceCommand::Microphone { session, audio: bytes.to_vec() }).await.is_err() { break; }
                    },
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {},
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => { voice_error(&mut socket, "Invalid voice audio frame.").await; break; },
                },
                _ = &mut idle => {
                    voice_error(&mut socket, "Voice connection lost. Courtside will resume the call.").await;
                    break;
                }
            }
        }
    }
    // Includes loss of Wi-Fi, screen/process interruption, and native Hang up.
    let _ = relay
        .send(VoiceCommand::Leave {
            session: Some(session),
            reply: None,
        })
        .await;
}

pub async fn events_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    user: AuthedUser,
) -> Response {
    ws.on_upgrade(move |socket| client_session(socket, state, user.0.id))
}

/// Audio is isolated from the ordinary event socket so 20 ms media frames do
/// not make status/chat delivery noisy. The client only opens this socket after
/// an explicit Live listen tap.
pub async fn call_audio_handler(
    Path(call_id): Path<Uuid>,
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    user: AuthedUser,
) -> Response {
    let allowed = crate::db::call_belongs_to_user(&state.db, call_id, user.0.id)
        .await
        .unwrap_or(false);
    if !allowed {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    }
    ws.on_upgrade(move |socket| audio_session(socket, state, call_id))
}

async fn audio_session(mut socket: WebSocket, state: AppState, call_id: Uuid) {
    let mut frames = state.call_audio.subscribe();
    loop {
        tokio::select! {
            frame = frames.recv() => {
                let Ok(frame) = frame else { break };
                if frame.call_id != call_id { continue; }
                if frame.payload.is_empty() { continue; }
                if socket.send(Message::text(frame.payload)).await.is_err() { break; }
            }
            message = socket.recv() => match message {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            }
        }
    }
}

/// Playback only: audio sources carry distinct track IDs so their playback
/// clocks run together. In particular, Twilio's continuous inbound silence
/// must never be queued behind generated assistant speech.
pub async fn call_listen_handler(
    Path(call_id): Path<Uuid>,
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    user: AuthedUser,
) -> Response {
    match crate::db::find_call_for_user(&state.db, call_id, user.0.id).await {
        Ok(Some(call)) if call.status == "in_progress" => {}
        Ok(Some(_)) => {
            return (
                axum::http::StatusCode::CONFLICT,
                "The call is not connected.",
            )
                .into_response();
        }
        _ => return axum::http::StatusCode::NOT_FOUND.into_response(),
    }
    ws.max_message_size(4096)
        .on_upgrade(move |socket| listen_session(socket, state, call_id))
}

async fn listen_session(mut socket: WebSocket, state: AppState, call_id: Uuid) {
    let mut frames = state.call_audio.subscribe();
    let mut events = state.events.subscribe();
    if socket
        .send(Message::text(r#"{"type":"connected"}"#))
        .await
        .is_err()
    {
        return;
    }
    loop {
        tokio::select! {
            frame = frames.recv() => {
                let frame = match frame {
                    Ok(frame) => frame,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        voice_error(&mut socket, "Live audio fell behind. Tap Live listen to reconnect.").await;
                        break;
                    }
                    Err(_) => break,
                };
                if frame.call_id != call_id { continue; }
                // A stalled listener must not receive seconds of stale speech
                // after it recovers. Assistant bursts are retained by the
                // player's own track, without delaying the caller's track.
                if frame.emitted_at.elapsed() > Duration::from_millis(500) {
                    voice_error(&mut socket, "Live audio fell behind. Tap Live listen to reconnect.").await;
                    break;
                }
                let Ok(audio) = BASE64.decode(&frame.payload) else { continue; };
                let mut packet = Vec::with_capacity(audio.len() + 1);
                packet.push(frame.track as u8);
                packet.extend_from_slice(&audio);
                match tokio::time::timeout(Duration::from_millis(500), socket.send(Message::Binary(packet.into()))).await {
                    Ok(Ok(())) => {},
                    _ => break,
                }
            }
            event = events.recv() => {
                if let Ok(ServerEvent::CallStatusChanged { call_id: id, status }) = event {
                    if id == call_id && matches!(status, shared::CallStatus::Completed | shared::CallStatus::Failed) {
                        let _ = socket.send(Message::text(r#"{"type":"ended","message":"The call ended."}"#)).await;
                        break;
                    }
                }
            }
            message = socket.recv() => match message {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {},
            }
        }
    }
}

async fn client_session(mut socket: WebSocket, state: AppState, user_id: Uuid) {
    let mut events = state.events.subscribe();
    loop {
        tokio::select! {
            event = events.recv() => {
                let Ok(event) = event else { break };
                let authorized = match &event {
                    ServerEvent::ChatActivity { chat_id, .. } => crate::db::history::belongs_to_user(&state.db, *chat_id, user_id).await,
                    _ => crate::db::call_belongs_to_user(&state.db, event_call_id(&event).expect("call event"), user_id).await,
                };
                match authorized {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(err) => {
                        tracing::warn!(?err, %user_id, "failed to authorize client event");
                        continue;
                    }
                }
                let Ok(json) = serde_json::to_string(&event) else { continue };
                if socket.send(Message::text(json)).await.is_err() {
                    break;
                }
            }
            message = socket.recv() => {
                match message {
                    Some(Ok(Message::Text(text))) => {
                        // Only Ping exists today; ignore anything unknown.
                        let _ = serde_json::from_str::<ClientEvent>(&text);
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
        }
    }
}

fn event_call_id(event: &ServerEvent) -> Option<Uuid> {
    Some(match event {
        ServerEvent::ChatActivity { .. } => return None,
        ServerEvent::CallStatusChanged { call_id, .. }
        | ServerEvent::SummaryReady { call_id, .. }
        | ServerEvent::ChatMessageAdded { call_id, .. }
        | ServerEvent::CallHandoffChanged { call_id, .. }
        | ServerEvent::CallQuestionChanged { call_id, .. }
        | ServerEvent::CallRecallChanged { call_id, .. }
        | ServerEvent::CallModeChanged { call_id, .. }
        | ServerEvent::TranscriptDelta { call_id, .. } => *call_id,
        ServerEvent::Transcript { segment } => segment.call_id,
        ServerEvent::IncomingCall { call, .. } => call.id,
    })
}

#[cfg(test)]
mod chat_event_tests {
    use super::*;
    use axum::{routing::any, Router};
    use futures_util::StreamExt;
    use tokio_tungstenite::connect_async;

    #[tokio::test]
    async fn chat_activity_is_delivered_only_to_its_chat_owner() {
        let db = crate::db::connect("sqlite::memory:").await.unwrap();
        let owner = crate::db::create_user(&db, "chat-event-owner@example.test", "Owner", "unused")
            .await
            .unwrap();
        let other = crate::db::create_user(&db, "chat-event-other@example.test", "Other", "unused")
            .await
            .unwrap();
        let chat = crate::db::ensure_current_chat(&db, owner.id).await.unwrap();
        let token = crate::db::insert_token(&db, owner.id).await.unwrap();
        let other_token = crate::db::insert_token(&db, other.id).await.unwrap();
        let state = AppState::new(db, crate::config::Config::for_test());
        let app = Router::new()
            .route("/api/ws", any(events_handler))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                super::super::auth::require_auth,
            ))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let (mut owner_socket, _) = connect_async(format!("ws://{address}/api/ws?token={token}"))
            .await
            .unwrap();
        let (mut other_socket, _) =
            connect_async(format!("ws://{address}/api/ws?token={other_token}"))
                .await
                .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while state.events.receiver_count() < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let event = ServerEvent::ChatActivity {
            chat_id: chat.id,
            request_id: Uuid::new_v4(),
            activity: Some(shared::ChatActivity::SearchingChats),
        };
        state.broadcast(event.clone());
        let received = tokio::time::timeout(Duration::from_secs(1), owner_socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<ServerEvent>(received.to_text().unwrap()).unwrap(),
            event
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(150), other_socket.next())
                .await
                .is_err()
        );
        let call = crate::db::create_call(
            &state.db,
            owner.id,
            shared::CallDirection::Outbound,
            "+14155550100",
            Some("Discuss a booking."),
        )
        .await
        .unwrap();
        let recall = ServerEvent::CallRecallChanged {
            call_id: call.id,
            remembering: true,
        };
        state.broadcast(recall.clone());
        let received = tokio::time::timeout(Duration::from_secs(1), owner_socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<ServerEvent>(received.to_text().unwrap()).unwrap(),
            recall
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(150), other_socket.next())
                .await
                .is_err()
        );
        owner_socket.close(None).await.unwrap();
        other_socket.close(None).await.unwrap();
        server.abort();
    }
}

#[cfg(test)]
mod voice_tests {
    use super::*;
    use axum::{routing::any, Router};
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{
        connect_async,
        tungstenite::{client::IntoClientRequest, Message as ClientMessage},
    };

    struct Fixture {
        state: AppState,
        call: Uuid,
        token: String,
        other_token: String,
        address: std::net::SocketAddr,
        server: tokio::task::JoinHandle<()>,
        user: crate::db::entities::users::Model,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    async fn fixture() -> Fixture {
        fixture_with_model(None).await
    }

    async fn fixture_with_model(model: Option<String>) -> Fixture {
        let db = crate::db::connect("sqlite::memory:").await.unwrap();
        let user = crate::db::create_user(&db, "voice@example.com", "Owner", "unused")
            .await
            .unwrap();
        let other = crate::db::create_user(&db, "other@example.com", "Other", "unused")
            .await
            .unwrap();
        // Equal numbers must not matter for an in-app audio participant.
        let peer = "+14155550100";
        crate::db::update_call_preferences(
            &db,
            user.clone(),
            shared::CallVoice::default(),
            true,
            None,
            Some(peer),
        )
        .await
        .unwrap();
        let call = crate::db::create_call(
            &db,
            user.id,
            shared::CallDirection::Outbound,
            peer,
            Some("Test voice"),
        )
        .await
        .unwrap();
        crate::db::set_call_status(&db, call.id, shared::CallStatus::InProgress)
            .await
            .unwrap();
        let token = crate::db::insert_token(&db, user.id).await.unwrap();
        let other_token = crate::db::insert_token(&db, other.id).await.unwrap();
        let mut config = crate::config::Config::for_test();
        config.test_realtime_url = model;
        let state = AppState::new(db, config);
        let router = Router::new()
            .route("/api/calls/{id}/voice", any(call_voice_handler))
            .route("/api/calls/{id}/listen", any(call_listen_handler))
            .route("/api/calls/{id}/audio", any(call_audio_handler))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                super::super::auth::require_auth,
            ))
            .route(
                "/twilio/media",
                any(crate::bridge::call_bridge::media_stream_handler),
            )
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Fixture {
            state,
            call: call.id,
            token,
            other_token,
            address,
            server,
            user,
        }
    }

    fn request(
        f: &Fixture,
        token: Option<&str>,
    ) -> tokio_tungstenite::tungstenite::http::Request<()> {
        let mut request = format!("ws://{}/api/calls/{}/voice", f.address, f.call)
            .into_client_request()
            .unwrap();
        if let Some(token) = token {
            request
                .headers_mut()
                .insert("Authorization", format!("Bearer {token}").parse().unwrap());
        }
        request
    }

    fn listen_request(
        f: &Fixture,
        token: Option<&str>,
    ) -> tokio_tungstenite::tungstenite::http::Request<()> {
        let mut request = request(f, token);
        *request.uri_mut() = format!("ws://{}/api/calls/{}/listen", f.address, f.call)
            .parse()
            .unwrap();
        request
    }

    #[tokio::test]
    async fn listen_requires_auth_ownership_and_a_connected_call() {
        let f = fixture().await;
        for (token, expected) in [(None, 401), (Some(f.other_token.as_str()), 404)] {
            let error = connect_async(listen_request(&f, token))
                .await
                .err()
                .unwrap();
            let tokio_tungstenite::tungstenite::Error::Http(response) = error else {
                panic!("expected HTTP rejection")
            };
            assert_eq!(response.status().as_u16(), expected);
        }
        crate::db::set_call_status(&f.state.db, f.call, shared::CallStatus::Completed)
            .await
            .unwrap();
        let error = connect_async(listen_request(&f, Some(&f.token)))
            .await
            .err()
            .unwrap();
        let tokio_tungstenite::tungstenite::Error::Http(response) = error else {
            panic!("expected completed-call rejection")
        };
        assert_eq!(response.status().as_u16(), 409);
    }

    #[tokio::test]
    async fn listen_labels_simultaneous_tracks_and_clears_without_attaching_a_microphone() {
        use crate::state::AudioTrack;
        let f = fixture().await;
        let (relay, mut commands) = mpsc::channel(16);
        f.state.register_voice_relay(f.call, relay).await;
        let (mut app, _) = connect_async(listen_request(&f, Some(&f.token)))
            .await
            .unwrap();
        let connected = app.next().await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(connected.to_text().unwrap()).unwrap()
                ["type"],
            "connected"
        );
        // Listening must remain playback-only, even if a client sends media.
        app.send(ClientMessage::Binary(vec![0x81; 160].into()))
            .await
            .unwrap();
        f.state.broadcast_audio(
            Uuid::new_v4(),
            AudioTrack::Callee,
            &BASE64.encode([0x82; 160]),
        );
        for (track, audio) in [
            (AudioTrack::Assistant, vec![0x80; 16000]),
            (AudioTrack::Callee, vec![0xff; 160]),
            (AudioTrack::User, vec![0x01; 160]),
            (AudioTrack::Assistant, vec![]),
        ] {
            f.state
                .broadcast_audio(f.call, track, &BASE64.encode(&audio));
            let frame = tokio::time::timeout(Duration::from_secs(1), app.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let mut expected = vec![track as u8];
            expected.extend(audio);
            assert_eq!(frame.into_data().as_ref(), expected);
        }
        assert!(commands.try_recv().is_err());
        f.state
            .set_status_and_broadcast(f.call, shared::CallStatus::Completed)
            .await;
        let ended = app.next().await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(ended.to_text().unwrap()).unwrap()["type"],
            "ended"
        );
    }

    #[tokio::test]
    async fn listen_rejects_stale_audio_instead_of_replaying_a_backlog() {
        let f = fixture().await;
        let (mut app, _) = connect_async(listen_request(&f, Some(&f.token)))
            .await
            .unwrap();
        let _ = app.next().await;
        f.state
            .call_audio
            .send(crate::state::CallAudioFrame {
                call_id: f.call,
                track: crate::state::AudioTrack::Callee,
                payload: BASE64.encode([0xff; 160]),
                emitted_at: std::time::Instant::now() - Duration::from_secs(2),
            })
            .unwrap();
        let error = tokio::time::timeout(Duration::from_secs(1), app.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let error: serde_json::Value = serde_json::from_str(error.to_text().unwrap()).unwrap();
        assert_eq!(error["type"], "error");
        assert!(error["message"].as_str().unwrap().contains("fell behind"));
    }

    #[tokio::test]
    async fn voice_requires_auth_ownership_and_a_connected_call() {
        let f = fixture().await;
        for (token, expected) in [(None, 401), (Some(f.other_token.as_str()), 404)] {
            let error = connect_async(request(&f, token)).await.err().unwrap();
            let tokio_tungstenite::tungstenite::Error::Http(response) = error else {
                panic!("expected HTTP rejection")
            };
            assert_eq!(response.status().as_u16(), expected);
        }
        crate::db::set_call_status(&f.state.db, f.call, shared::CallStatus::Completed)
            .await
            .unwrap();
        let error = connect_async(request(&f, Some(&f.token)))
            .await
            .err()
            .unwrap();
        let tokio_tungstenite::tungstenite::Error::Http(response) = error else {
            panic!("expected completed-call rejection")
        };
        assert_eq!(response.status().as_u16(), 409);
    }

    #[tokio::test]
    async fn audio_waits_for_callkit_ready_and_relays_both_directions_without_dialing() {
        let f = fixture().await;
        let (relay, mut commands) = mpsc::channel(16);
        f.state.register_voice_relay(f.call, relay).await;
        let (mut app, _) = connect_async(request(&f, Some(&f.token))).await.unwrap();
        assert!(commands.try_recv().is_err());
        app.send(ClientMessage::text(r#"{"type":"ready"}"#))
            .await
            .unwrap();
        let VoiceCommand::Join {
            session,
            output,
            reply,
        } = tokio::time::timeout(Duration::from_secs(1), commands.recv())
            .await
            .unwrap()
            .unwrap()
        else {
            panic!("expected join")
        };
        reply.send(Ok(())).unwrap();
        let ack = app.next().await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(ack.to_text().unwrap()).unwrap()["type"],
            "connected"
        );
        let microphone = vec![0x81; 160];
        app.send(ClientMessage::Binary(microphone.clone().into()))
            .await
            .unwrap();
        let VoiceCommand::Microphone {
            session: actual,
            audio,
        } = commands.recv().await.unwrap()
        else {
            panic!("expected microphone")
        };
        assert_eq!(actual, session);
        assert_eq!(audio, microphone);
        let callee = vec![0xf1; 160];
        output
            .send(VoiceOutput::Audio(callee.clone()))
            .await
            .unwrap();
        let received = app.next().await.unwrap().unwrap();
        assert_eq!(received.into_data().as_ref(), callee);
        app.close(None).await.unwrap();
        let VoiceCommand::Leave {
            session: leaving, ..
        } = tokio::time::timeout(Duration::from_secs(1), commands.recv())
            .await
            .unwrap()
            .unwrap()
        else {
            panic!("disconnect must request AI resume")
        };
        assert_eq!(leaving, Some(session));
    }

    #[tokio::test]
    async fn invalid_audio_disconnects_only_its_own_participant() {
        let f = fixture().await;
        let (relay, mut commands) = mpsc::channel(16);
        f.state.register_voice_relay(f.call, relay).await;
        let (mut app, _) = connect_async(request(&f, Some(&f.token))).await.unwrap();
        app.send(ClientMessage::text(r#"{"type":"ready"}"#))
            .await
            .unwrap();
        let VoiceCommand::Join {
            session,
            output: _output,
            reply,
        } = commands.recv().await.unwrap()
        else {
            panic!("join")
        };
        reply.send(Ok(())).unwrap();
        app.next().await.unwrap().unwrap();
        app.send(ClientMessage::Binary(vec![0; 1601].into()))
            .await
            .unwrap();
        let error = app.next().await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(error.to_text().unwrap()).unwrap()["type"],
            "error"
        );
        let VoiceCommand::Leave {
            session: leaving, ..
        } = commands.recv().await.unwrap()
        else {
            panic!("leave")
        };
        assert_eq!(leaving, Some(session));
    }

    #[tokio::test]
    async fn return_to_ai_releases_control_lock_and_uses_the_existing_audio_stream() {
        let f = fixture().await;
        f.state
            .set_call_mode(f.call, shared::CallMode::InAppTakeover)
            .await;
        let (relay, mut commands) = mpsc::channel(16);
        f.state.register_voice_relay(f.call, relay).await;
        let state = f.state.clone();
        let id = f.call;
        let user = f.user.clone();
        let returned = tokio::spawn(async move {
            crate::api::routes::resume_assistant(State(state), AuthedUser(user), Path(id)).await
        });
        let VoiceCommand::Leave {
            session,
            reply: Some(reply),
        } = commands.recv().await.unwrap()
        else {
            panic!("native return command")
        };
        assert_eq!(session, None);
        let lock = f.state.call_control_lock(f.call).await;
        let _guard = tokio::time::timeout(Duration::from_secs(1), lock.lock())
            .await
            .expect("REST must not hold the lock while waiting for media");
        f.state
            .set_call_control(
                f.call,
                shared::CallMode::Assistant,
                "Courtside has the call again.",
            )
            .await;
        reply.send(Ok(())).unwrap();
        assert_eq!(
            returned.await.unwrap().unwrap().0.mode,
            shared::CallMode::Assistant
        );
        // No Twilio SID or second phone exists in this fixture.
    }

    #[tokio::test]
    async fn old_bridge_cleanup_cannot_remove_the_new_relay() {
        let f = fixture().await;
        let (old, _old_receiver) = mpsc::channel(1);
        let old_id = f.state.register_voice_relay(f.call, old).await;
        let (new, mut new_receiver) = mpsc::channel(1);
        f.state.register_voice_relay(f.call, new).await;
        f.state.unregister_voice_relay(f.call, old_id).await;
        f.state
            .voice_relay(f.call)
            .await
            .unwrap()
            .send(VoiceCommand::Leave {
                session: None,
                reply: None,
            })
            .await
            .unwrap();
        assert!(matches!(
            new_receiver.recv().await,
            Some(VoiceCommand::Leave { .. })
        ));
    }

    #[tokio::test]
    async fn complete_takeover_and_return_keep_the_same_twilio_socket_and_conversation() {
        use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
        // Everything here is a local socket fixture. No Twilio REST calls or
        // real model requests, and no actual phone numbers are dialed.
        let (starts, mut model_starts) = mpsc::unbounded_channel::<(bool, String)>();
        let model = Router::new().route("/live", any(move |ws: WebSocketUpgrade| {
            let starts = starts.clone();
            async move { ws.on_upgrade(move |mut socket| async move {
                let mut assistant = false;
                let mut user_track = false;
                let mut transcribed = false;
                while let Some(Ok(Message::Text(text))) = socket.recv().await {
                    let event: serde_json::Value = serde_json::from_str(&text).unwrap();
                    match event["type"].as_str().unwrap_or_default() {
                        "session.start" => {
                            assistant = event["session"]["delegation"].is_object() || event["session"]["instructions"].as_str().unwrap_or_default().contains("presence check");
                            let instructions = event["session"]["instructions"].as_str().unwrap_or_default().to_string();
                            user_track = instructions.contains("microphone belongs to: User");
                            let _ = starts.send((assistant, instructions));
                            socket.send(Message::text(r#"{"type":"session.started"}"#)).await.unwrap();
                            if assistant {
                                socket.send(Message::text(serde_json::json!({
                                    "type":"session.output_audio.delta", "delta":BASE64.encode(vec![0x22; 160])
                                }).to_string())).await.unwrap();
                            }
                        },
                        "session.input_audio.append" if !assistant && !transcribed => {
                            transcribed = true;
                            socket.send(Message::text(serde_json::json!({
                                "type":"session.input_transcript.delta", "delta": if user_track { "Book it for Friday." } else { "Friday is available." },
                                "start_ms":0, "end_ms":1000
                            }).to_string())).await.unwrap();
                        },
                        "session.close" => {
                            let _ = socket.send(Message::text(r#"{"type":"session.closed"}"#)).await;
                            break;
                        },
                        _ => {},
                    }
                }
            }) }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let model_server = tokio::spawn(async move {
            axum::serve(listener, model).await.unwrap();
        });
        let f = fixture_with_model(Some(format!("ws://{address}/live"))).await;
        // Don't request a post-call summary from an external model in a test.
        f.state.begin_summary(f.call).await;
        let mut events = f.state.events.subscribe();
        let (mut twilio, _) = connect_async(format!("ws://{}/twilio/media", f.address))
            .await
            .unwrap();
        twilio.send(ClientMessage::text(serde_json::json!({"event":"start", "start":{
            "streamSid":"MZsame", "callSid":"CAfixture", "customParameters":{"call_id":f.call.to_string()}
        }}).to_string())).await.unwrap();
        let opening = tokio::time::timeout(Duration::from_secs(2), twilio.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(opening.to_text().unwrap()).unwrap()["event"],
            "media"
        );
        let (mut app, _) = connect_async(request(&f, Some(&f.token))).await.unwrap();
        app.send(ClientMessage::text(r#"{"type":"ready"}"#))
            .await
            .unwrap();
        let clear = twilio.next().await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(clear.to_text().unwrap()).unwrap()["event"],
            "clear"
        );
        let ack = tokio::time::timeout(Duration::from_secs(3), app.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(ack.to_text().unwrap()).unwrap()["type"],
            "connected"
        );
        assert_eq!(
            f.state.call_mode(f.call).await,
            shared::CallMode::InAppTakeover
        );

        // A second app connection cannot steal the speaking role.
        let (mut duplicate, _) = connect_async(request(&f, Some(&f.token))).await.unwrap();
        duplicate
            .send(ClientMessage::text(r#"{"type":"ready"}"#))
            .await
            .unwrap();
        let rejected = duplicate.next().await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(rejected.to_text().unwrap()).unwrap()["type"],
            "error"
        );

        let microphone = vec![0x81; 160];
        app.send(ClientMessage::Binary(microphone.clone().into()))
            .await
            .unwrap();
        let human = twilio.next().await.unwrap().unwrap();
        let human: serde_json::Value = serde_json::from_str(human.to_text().unwrap()).unwrap();
        assert_eq!(human["streamSid"], "MZsame");
        assert_eq!(
            BASE64
                .decode(human["media"]["payload"].as_str().unwrap())
                .unwrap(),
            microphone
        );
        let callee = vec![0xf1; 160];
        twilio
            .send(ClientMessage::text(
                serde_json::json!({"event":"media", "media":{"payload":BASE64.encode(&callee)}})
                    .to_string(),
            ))
            .await
            .unwrap();
        let incoming = app.next().await.unwrap().unwrap();
        assert_eq!(incoming.into_data().as_ref(), callee); // never the user's microphone
        tokio::time::timeout(Duration::from_secs(2), async {
            let mut speakers = std::collections::HashSet::new();
            while speakers.len() < 2 {
                if let Ok(ServerEvent::TranscriptDelta { speaker, .. }) = events.recv().await {
                    speakers.insert(format!("{speaker:?}"));
                }
            }
        })
        .await
        .expect("both sides must be observed with separate speaker labels");

        app.close(None).await.unwrap();
        // The same Twilio connection clears human speech, then resumes AI.
        let clear = tokio::time::timeout(Duration::from_secs(4), twilio.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(clear.to_text().unwrap()).unwrap()["event"],
            "clear"
        );
        let resumed = twilio.next().await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(resumed.to_text().unwrap()).unwrap()
                ["streamSid"],
            "MZsame"
        );
        assert_eq!(f.state.call_mode(f.call).await, shared::CallMode::Assistant);
        let mut restored_context = None;
        while let Ok((assistant, instructions)) = model_starts.try_recv() {
            if assistant && instructions.contains("Book it for Friday.") {
                restored_context = Some(instructions);
            }
        }
        assert!(restored_context.unwrap().contains("Friday is available."));

        // Join again, then the other person's hangup ends native participation.
        let (mut app, _) = connect_async(request(&f, Some(&f.token))).await.unwrap();
        app.send(ClientMessage::text(r#"{"type":"ready"}"#))
            .await
            .unwrap();
        twilio.next().await.unwrap().unwrap(); // clear
        app.next().await.unwrap().unwrap(); // connected
        twilio
            .send(ClientMessage::text(r#"{"event":"stop"}"#))
            .await
            .unwrap();
        let ended = tokio::time::timeout(Duration::from_secs(4), app.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(ended.to_text().unwrap()).unwrap()["type"],
            "ended"
        );
        model_server.abort();
    }
}
