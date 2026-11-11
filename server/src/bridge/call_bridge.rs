//! The per-call bridge: pumps audio between a Twilio Media Stream socket and
//! an OpenAI Realtime session, persisting/broadcasting the transcript.

use axum::extract::ws::{Message as AxumMessage, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use futures_util::{SinkExt, StreamExt};
use shared::{CallMode, CallStatus, ServerEvent, Speaker};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;
use uuid::Uuid;

use super::{
    lifecycle::{audio_is_audible, CallEnding, CallSilence, EndCallState, SilenceAction},
    openai,
    startup::{ReplayedReply, ReplyAudio, StartupHello},
    twilio_ws,
    voice::{owns_session, VoiceCommand, VoiceOutput, VoicePeer, VoiceResult},
};
use crate::state::{AppState, AudioTrack, CALL_QUESTION_WAIT_SECONDS};

#[derive(Debug)]
struct TranscriptFragment {
    speaker: Speaker,
    text: String,
    start_ms: i64,
    end_ms: i64,
}

const END_CALL_MARK: &str = "courtside_end_call_played";
const SEMANTIC_END_CHECK_DELAY: std::time::Duration = std::time::Duration::from_millis(1_500);
const AUDIO_CLOCK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);
// Live permits a startup burst up to five seconds. Leave a one-second
// margin, and keep any longer reply paced rather than dropping its words.
const REPLY_STARTUP_BURST_SAMPLES: usize = 8_000 * 4;
const CALL_QUESTION_REMINDER_SECONDS: u64 = CALL_QUESTION_WAIT_SECONDS / 2;

/// Connecting the task session must not block phone input or native takeover.
/// Dropping a call or taking it over also cancels an unfinished connection.
struct AnsweredSessionJob {
    task: tokio::task::JoinHandle<anyhow::Result<(openai::OpenAiSink, openai::OpenAiStream)>>,
}

impl AnsweredSessionJob {
    fn new(config: std::sync::Arc<crate::config::Config>, start: serde_json::Value) -> Self {
        Self {
            task: tokio::spawn(async move {
                for attempt in 0..3 {
                    match openai::connect_started(&config, start.clone()).await {
                        Ok(session) => return Ok(session),
                        Err(error) if attempt == 2 => return Err(error),
                        Err(_) => tokio::time::sleep(std::time::Duration::from_secs(1)).await,
                    }
                }
                unreachable!()
            }),
        }
    }
}

impl Drop for AnsweredSessionJob {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn wait_for_answered_session(
    job: &mut Option<AnsweredSessionJob>,
) -> anyhow::Result<(openai::OpenAiSink, openai::OpenAiStream)> {
    match job {
        Some(job) => (&mut job.task)
            .await
            .map_err(|error| anyhow::anyhow!("task session connection failed: {error}"))?,
        None => std::future::pending().await,
    }
}

/// The media loop keeps running while Luna reads history. Dropping the job
/// cancels HTTP work on an owner reply, timeout, takeover, or disconnect.
struct CallRecallJob {
    question_id: Uuid,
    task: tokio::task::JoinHandle<anyhow::Result<Option<String>>>,
}
impl Drop for CallRecallJob {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn wait_for_recall(
    job: &mut Option<CallRecallJob>,
) -> (Uuid, anyhow::Result<Option<String>>) {
    match job {
        Some(job) => (
            job.question_id,
            (&mut job.task)
                .await
                .map_err(|error| anyhow::anyhow!("history lookup failed: {error}"))
                .and_then(|result| result),
        ),
        None => std::future::pending().await,
    }
}

fn semantic_transcript_preview(fragments: &[TranscriptFragment]) -> Option<String> {
    let has_assistant = fragments
        .iter()
        .any(|fragment| fragment.speaker == Speaker::Assistant && !fragment.text.trim().is_empty());
    if !has_assistant {
        return None;
    }

    let mut turns: Vec<(Speaker, String)> = Vec::new();
    for fragment in fragments {
        if fragment.text.trim().is_empty() || fragment.speaker == Speaker::Action {
            continue;
        }
        if let Some((speaker, text)) = turns.last_mut() {
            if *speaker == fragment.speaker {
                text.push_str(&fragment.text);
                continue;
            }
        }
        turns.push((fragment.speaker, fragment.text.clone()));
    }
    let transcript = turns
        .into_iter()
        .map(|(speaker, text)| {
            let speaker = match speaker {
                Speaker::Assistant => "Courtside",
                Speaker::Callee => "Other party",
                Speaker::User => "User",
                Speaker::Action => "Call action",
            };
            format!("{speaker}: {}", text.trim())
        })
        .collect::<Vec<_>>()
        .join("\n");
    let character_count = transcript.chars().count();
    let preview = if character_count > 6_000 {
        transcript
            .chars()
            .skip(character_count - 6_000)
            .collect::<String>()
    } else {
        transcript
    };
    Some(preview)
}

fn validated_dtmf_digits(raw: &str) -> Option<String> {
    let digits = raw.trim();
    (!digits.is_empty()
        && digits.chars().count() <= 6
        && digits
            .chars()
            .all(|character| character.is_ascii_digit() || matches!(character, '*' | '#')))
    .then(|| digits.to_string())
}

fn concise_menu_reason(reason: &str) -> String {
    reason
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(80)
        .collect()
}

pub async fn media_stream_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| async move {
        if let Err(err) = run(socket, state).await {
            tracing::error!(?err, "call bridge ended with error");
        }
    })
}

pub async fn observe_stream_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> Response {
    ws.on_upgrade(move |socket| async move {
        if let Err(err) = observe_run(socket, state).await {
            tracing::error!(?err, "takeover observer ended with error");
        }
    })
}

async fn run(mut socket: WebSocket, state: AppState) -> anyhow::Result<()> {
    // Twilio sends `connected` then `start`; wait for `start` to learn which
    // call this stream belongs to.
    let start = loop {
        let Some(message) = socket.recv().await else {
            anyhow::bail!("twilio socket closed before start frame");
        };
        if let AxumMessage::Text(text) = message? {
            match serde_json::from_str::<twilio_ws::TwilioInbound>(&text) {
                Ok(twilio_ws::TwilioInbound::Start { start }) => break start,
                Ok(_) => continue,
                Err(err) => tracing::warn!(?err, "unrecognized twilio frame before start"),
            }
        }
    };

    let call_id: Uuid = start
        .custom_parameters
        .get("call_id")
        .and_then(|raw| raw.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("start frame missing call_id parameter"))?;
    let stream_sid = start.stream_sid.clone();
    let resumed = start
        .custom_parameters
        .get("resumed")
        .is_some_and(|value| value == "true");
    let dtmf_resume_digits = start
        .custom_parameters
        .get("resume_reason")
        .is_some_and(|value| value == "dtmf")
        .then(|| start.custom_parameters.get("pressed_digits").cloned())
        .flatten();
    tracing::info!(%call_id, %stream_sid, call_sid = %start.call_sid, "media stream started");

    let call = crate::db::find_call(&state.db, call_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("unknown call {call_id}"))?;
    state
        .set_status_and_broadcast(call_id, CallStatus::InProgress)
        .await;
    let connected_at = std::time::Instant::now();
    if let (Some(cap), Some(call_sid)) = (call.quota_cap_seconds, call.twilio_call_sid.clone()) {
        let config = state.config.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(cap.max(1) as u64)).await;
            if let Err(err) = crate::twilio::rest::hangup_call(&config, &call_sid).await {
                tracing::warn!(?err, %call_sid, "quota hangup was not needed or failed");
            }
        });
    }

    let inbound = call.direction == "inbound";
    let user = match call.user_id {
        Some(user_id) => crate::db::find_user_by_id(&state.db, user_id).await?,
        None => None,
    };
    let user_name = call
        .share_user_name
        .then(|| user.as_ref().map(|user| user.display_name.as_str()))
        .flatten();
    let custom_instructions = user
        .as_ref()
        .and_then(|user| user.custom_call_instructions.as_deref());
    let voice = user
        .as_ref()
        .map(|user| crate::db::call_preferences(user).voice)
        .unwrap_or_default();
    let mut instructions = openai::call_instructions(
        call.task_prompt.as_deref(),
        inbound,
        user_name,
        call.announce_courtside,
        custom_instructions,
    );
    instructions.push_str(&openai::call_style_instructions(call_id));
    let base_instructions = instructions.clone();
    let mut awaiting_recipient = !inbound && !resumed;
    let mut real_recipient_audio = false;
    if resumed {
        let prior = transcript_context(&state, call_id).await?;
        if !prior.is_empty() {
            instructions.push_str("\n\nConversation before this audio bridge resumed:\n");
            instructions.push_str(&prior);
        }
    }

    let opening = if let Some(digits) = dtmf_resume_digits.as_deref() {
        openai::dtmf_resume_request(digits)
    } else if resumed {
        openai::resume_request()
    } else if awaiting_recipient {
        openai::answer_check_request()
    } else {
        openai::greeting_request(inbound, call.announce_courtside, user_name)
    };
    if !resumed {
        instructions = if awaiting_recipient {
            openai::awaiting_recipient_instructions()
        } else {
            openai::initial_call_instructions(&instructions, &opening)
        };
    }
    let (mut ai_sink, mut ai_stream) = openai::connect(&state.config).await?;
    ai_sink
        .send(TungsteniteMessage::text(
            if awaiting_recipient {
                openai::presence_session_start(&state.config, voice)
            } else {
                openai::call_session_start(&state.config, &instructions, voice, inbound)
            }
            .to_string(),
        ))
        .await?;
    wait_for_live_start(&mut ai_stream).await?;
    ai_sink
        .send(TungsteniteMessage::text(opening.to_string()))
        .await?;
    // Some carrier streams omit quiet packets. GPT-Live still needs audio
    // time to advance before anyone says hello; digital silence is sufficient.
    let silence_payload = BASE64.encode([0xff; 160]); // 20 ms at 8 kHz
    ai_sink
        .send(TungsteniteMessage::text(
            openai::input_audio_append(&silence_payload).to_string(),
        ))
        .await?;
    let mut input_clock = super::input_clock::InputClock::default();
    let mut startup_hello = StartupHello::new(resumed);
    startup_hello.record_input(&silence_payload);
    let (live_input_sender, mut live_input_receiver) = tokio::sync::mpsc::unbounded_channel();
    state
        .register_live_call_input(call_id, live_input_sender.clone())
        .await;
    let mut transcript_fragments = Vec::new();
    let mut transcript_offset_ms = 0_i64;
    let mut reply_audio = ReplyAudio::default();
    let mut replayed_reply = ReplayedReply::default();
    let mut answered_session = None;
    let (voice_sender, mut voice_commands) = mpsc::channel(64);
    let voice_bridge = state.register_voice_relay(call_id, voice_sender).await;
    let _voice_registration = VoiceRegistration {
        state: state.clone(),
        call_id,
        bridge: voice_bridge,
    };
    let mut live_session_closed = false;
    let mut ending = CallEnding::new();
    let mut redirecting_for_dtmf = false;
    let mut recall_job = None;
    let mut private_question = state.pending_call_question(call_id).await;
    let mut private_question_pending = private_question.is_some();
    let mut private_question_checkpoint = CALL_QUESTION_REMINDER_SECONDS;
    let mut pending_handoff = state.call_handoff(call_id).await;
    let mut transcript_revision = 0_u64;
    let mut semantic_check_in_flight = false;
    let mut semantic_recheck_pending = false;
    let mut semantic_failures = 0_u8;
    let mut pending_responses = std::collections::HashMap::new();
    let mut last_caller_audio = None;
    let mut last_assistant_audio = None;
    let mut reply_waiting = false;
    let mut silence = CallSilence::new(tokio::time::Instant::now());
    let (semantic_result_sender, mut semantic_result_receiver) =
        tokio::sync::mpsc::unbounded_channel::<(u64, Result<Option<String>, String>)>();
    let end_call_timer = tokio::time::sleep(std::time::Duration::from_secs(86_400));
    tokio::pin!(end_call_timer);
    let semantic_end_timer = tokio::time::sleep(std::time::Duration::from_secs(86_400));
    tokio::pin!(semantic_end_timer);
    let private_question_timer = tokio::time::sleep_until(
        private_question
            .as_ref()
            .map(|pending| pending.deadline)
            .unwrap_or_else(|| {
                tokio::time::Instant::now() + std::time::Duration::from_secs(86_400)
            }),
    );
    tokio::pin!(private_question_timer);
    let mut audio_clock_timer = super::input_clock::paced_timer();
    let mut silence_timer = tokio::time::interval(std::time::Duration::from_secs(1));
    silence_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut opening_spoken = resumed;
    let mut recipient_spoken = false;
    let opening_timer = tokio::time::sleep(std::time::Duration::from_millis(350));
    tokio::pin!(opening_timer);
    let mut opening_audio_timer = tokio::time::interval(std::time::Duration::from_millis(20));
    opening_audio_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            Some(command) = voice_commands.recv() => {
                match command {
                    VoiceCommand::Join { session, output, reply } => {
                        if reply.is_closed() || output.is_closed() { continue; }
                        let control_lock = state.call_control_lock(call_id).await;
                        let control_guard = control_lock.lock().await;
                        if state.call_mode(call_id).await != CallMode::Assistant {
                            let _ = reply.send(Err("Someone is already taking over this call.".into()));
                            continue;
                        }
                        state.clear_call_handoff(call_id, None).await;
                        pending_handoff = None;
                        state.clear_call_question(call_id).await;
                        recall_job.take();
                        private_question = None;
                        private_question_pending = false;
                        // Discard queued AI speech before the human microphone
                        // can reach the callee. No phone leg or Twilio redirect.
                        startup_hello.stop();
                        answered_session.take();
                        let clear = twilio_ws::TwilioOutbound::clear(&stream_sid);
                        socket.send(AxumMessage::text(serde_json::to_string(&clear)?)).await?;
                        state.broadcast_audio(call_id, AudioTrack::Assistant, "");
                        state.set_call_control(call_id, CallMode::InAppTakeover,
                            "You’re on the call. Use Return to AI when you’re done.").await;
                        drop(control_guard);
                        let _ = ai_sink.send(TungsteniteMessage::text(openai::session_close().to_string())).await;
                        collect_live_close_events(&mut ai_stream, &mut transcript_fragments, &mut startup_hello,
                            real_recipient_audio || !awaiting_recipient, transcript_offset_ms, &mut replayed_reply).await;
                        let _ = ai_sink.close().await;
                        live_session_closed = true;
                        persist_transcript_fragments(&state, call_id, std::mem::take(&mut transcript_fragments)).await;
                        ending = CallEnding::new();
                        semantic_recheck_pending = false;
                        transcript_revision = transcript_revision.saturating_add(1);
                        let mut participant = Some(VoicePeer { session, output });
                        let _ = reply.send(Ok(()));
                        let mut returned_replies = Vec::new();
                        loop {
                            if let Some(peer) = participant.take() {
                                match native_takeover(&mut socket, &mut voice_commands, &state, call_id, &stream_sid, peer).await? {
                                    NativeExit::CallEnded => break,
                                    NativeExit::Return(reply) => {
                                        if let Some(reply) = reply { returned_replies.push(reply); }
                                    }
                                }
                            }
                            match restore_assistant(&mut socket, &mut voice_commands, &state, call_id,
                                &stream_sid, &base_instructions, voice, inbound, &mut returned_replies).await? {
                                RestoreExit::Ready(sink, stream) => {
                                    ai_sink = sink;
                                    ai_stream = stream;
                                    live_session_closed = false;
                                    break;
                                }
                                RestoreExit::Joined(peer) => participant = Some(peer),
                                RestoreExit::CallEnded => break,
                            }
                        }
                        // A human disconnect returns to the assistant. A
                        // callee disconnect ends the parent call normally.
                        if live_session_closed { break; }
                        private_question = state.pending_call_question(call_id).await;
                        private_question_pending = private_question.is_some();
                        private_question_checkpoint = CALL_QUESTION_REMINDER_SECONDS;
                        if let Some(pending) = private_question.as_ref() {
                            private_question_timer.as_mut().reset(pending.deadline);
                        }
                        pending_responses.clear();
                        opening_spoken = true; // returning to an existing conversation
                        awaiting_recipient = false;
                        input_clock.clear();
                        startup_hello = StartupHello::new(true);
                        reply_audio = ReplyAudio::default();
                        replayed_reply = ReplayedReply::default();
                        transcript_offset_ms = connected_at.elapsed().as_millis().min(i64::MAX as u128) as i64;
                        silence.reset(tokio::time::Instant::now());
                        audio_clock_timer.reset_after(AUDIO_CLOCK_INTERVAL);
                    }
                    VoiceCommand::Leave { reply, .. } => {
                        if let Some(reply) = reply { let _ = reply.send(Ok(())); }
                    }
                    VoiceCommand::Microphone { .. } => {}, // stale session
                    VoiceCommand::DeclineHandoff { request_id, reply } => {
                        if reply.is_closed() { continue; }
                        if state.clear_call_handoff(call_id, Some(request_id)).await {
                            pending_handoff = None;
                            silence.reset(tokio::time::Instant::now());
                            // This is trusted application state, not a private
                            // fact supplied by the caller or a fake transcript.
                            ai_sink.send(TungsteniteMessage::text(openai::incoming_handoff_unavailable().to_string())).await?;
                            crate::bridge::incoming::unavailable(&state, call_id).await;
                            let _ = reply.send(Ok(()));
                        } else {
                            let _ = reply.send(Err("That invitation is no longer pending.".into()));
                        }
                    }
                }
            }
            (question_id, result) = wait_for_recall(&mut recall_job) => {
                recall_job.take();
                state.set_call_remembering(call_id, false).await;
                if ending.state != EndCallState::Active { continue; }
                let Some(pending) = private_question.as_ref().filter(|pending| pending.request.id == question_id) else { continue };
                let answer = match result {
                    Ok(answer) => answer,
                    Err(error) => { tracing::warn!(%call_id, ?error, "past-chat lookup unavailable; owner can still reply"); None }
                };
                if let Some(answer) = answer {
                    if !crate::db::history_settings::enabled(&state.db, call.user_id.expect("owned call")).await.unwrap_or(false)
                        || !state.resolve_call_question_from_history(call_id, question_id).await { continue; }
                    let question = pending.request.question.clone();
                    let chat_id = pending.request.chat_id;
                    private_question = None;
                    private_question_pending = false;
                    silence.reset(tokio::time::Instant::now());
                    semantic_recheck_pending = true;
                    semantic_end_timer.as_mut().reset(tokio::time::Instant::now() + SEMANTIC_END_CHECK_DELAY);
                    ai_sink.send(TungsteniteMessage::text(openai::backend_history_answer(&question, &answer).to_string())).await?;
                    for update in openai::live_history_answer(&question, &answer) {
                        ai_sink.send(TungsteniteMessage::text(update.to_string())).await?;
                    }
                    if let Some(user_id) = call.user_id {
                        let content = format!("I found the detail in an earlier chat and passed it to the call: {answer}");
                        if let Ok(message) = crate::db::add_assistant_chat_update(&state.db, user_id, chat_id, &content).await {
                            state.broadcast(ServerEvent::ChatMessageAdded {call_id,message});
                        }
                    }
                }
            }
            Some(answer) = live_input_receiver.recv() => {
                if !private_question.as_ref().is_some_and(|pending| pending.request.id == answer.question.id) {
                    continue; // A takeover or closing canceled this specific question.
                }
                recall_job.take();
                state.set_call_remembering(call_id, false).await;
                tracing::info!(%call_id, "injecting private user answer into live call");
                private_question = None;
                private_question_pending = false;
                silence.reset(tokio::time::Instant::now());
                semantic_recheck_pending = true;
                semantic_end_timer.as_mut().reset(tokio::time::Instant::now() + SEMANTIC_END_CHECK_DELAY);
                ai_sink.send(TungsteniteMessage::text(
                    openai::backend_user_answer(&answer.question.question, &answer.answer).to_string(),
                )).await?;
                for update in openai::live_user_answer(&answer.question.question, &answer.answer) {
                    ai_sink.send(TungsteniteMessage::text(update.to_string())).await?;
                }
            }
            twilio_message = socket.recv() => {
                let Some(twilio_message) = twilio_message else { break };
                let AxumMessage::Text(text) = twilio_message? else { continue };
                match serde_json::from_str::<twilio_ws::TwilioInbound>(&text) {
                    Ok(twilio_ws::TwilioInbound::Media { media }) => {
                        state.broadcast_audio(call_id, AudioTrack::Callee, &media.payload);
                        if ending.state == EndCallState::Active {
                            if awaiting_recipient { reply_audio.record(&media.payload); }
                            if audio_is_audible(&media.payload) {
                                last_caller_audio = Some(tokio::time::Instant::now());
                                reply_waiting = true;
                                real_recipient_audio = true;
                                silence.recipient_activity(tokio::time::Instant::now());
                                startup_hello.stop();
                            }
                            // While the cue is being inserted, replace quiet input
                            // with its frames instead of interleaving two clocks.
                            if !startup_hello.active() { input_clock.push(&media.payload); }
                        }
                    }
                    Ok(twilio_ws::TwilioInbound::Stop {}) => break,
                    Ok(twilio_ws::TwilioInbound::Dtmf { dtmf }) => {
                        tracing::debug!(%call_id, digit = %dtmf.digit, "dtmf received");
                    }
                    Ok(twilio_ws::TwilioInbound::Mark { mark })
                        if ending.state == EndCallState::WaitingForPlayback
                            && mark.name == END_CALL_MARK =>
                    {
                        tracing::info!(%call_id, "closing audio finished; ending Twilio call");
                        if let Err(err) = crate::twilio::rest::hangup_call(&state.config, &start.call_sid).await {
                            tracing::warn!(?err, call_sid = %start.call_sid, "agent end_call failed");
                        }
                        break;
                    }
                    Ok(_) => {}
                    Err(err) => tracing::warn!(?err, "unrecognized twilio frame"),
                }
            }
            ai_message = ai_stream.next() => {
                let Some(ai_message) = ai_message else {
                    tracing::warn!(%call_id, "openai stream closed");
                    break;
                };
                let TungsteniteMessage::Text(text) = ai_message? else { continue };
                match openai::parse_event(&text) {
                    openai::RealtimeEvent::AudioDelta(delta) => {
                        // Nothing may be queued behind the final playback marker.
                        if ending.state == EndCallState::WaitingForPlayback { continue; }
                        if super::lifecycle::audio_is_digital_silence(&delta) { continue; }
                        let audible = silence.assistant_audio(tokio::time::Instant::now(), &delta);
                        if audible {
                            let now = tokio::time::Instant::now();
                            if reply_waiting && last_assistant_audio.is_none_or(|at:tokio::time::Instant|now.duration_since(at)>std::time::Duration::from_millis(300)) {
                                if let Some(caller_at) = last_caller_audio {
                                    tracing::info!(%call_id, response_gap_ms = now.duration_since(caller_at).as_millis(),
                                        input_buffer_ms = input_clock.queued_ms(), pending_backend = pending_responses.len(),
                                        "assistant reply audio forwarded");
                                }
                                reply_waiting = false;
                            }
                            last_assistant_audio = Some(now);
                        }
                        if audible && !opening_spoken { tracing::info!(%call_id, elapsed_ms = connected_at.elapsed().as_millis(), "first greeting audio reached phone"); }
                        opening_spoken |= audible;
                        if audible {
                            startup_hello.stop();
                        }
                        state.broadcast_audio(call_id, AudioTrack::Assistant, &delta);
                        let frame = twilio_ws::TwilioOutbound::media(&stream_sid, delta);
                        socket.send(AxumMessage::text(serde_json::to_string(&frame)?)).await?;
                        if audible {
                            if let Some(deadline) = ending.speech(tokio::time::Instant::now()) {
                                end_call_timer.as_mut().reset(deadline);
                            }
                        }
                    }
                    openai::RealtimeEvent::InputTranscript { text, start_ms, end_ms } => {
                        if text.trim().is_empty() { continue; }
                        if startup_hello.discard_transcript(&text, start_ms, end_ms) { continue; }
                        // A caption from injected audio cannot release the waiting
                        // stage, even if recognition or its timestamps differ.
                        if awaiting_recipient && !real_recipient_audio { continue; }
                        let Some(text) = replayed_reply.filter(&text, end_ms) else { continue; };
                        recipient_spoken = true;
                        startup_hello.stop();
                        if awaiting_recipient && answered_session.is_none() && ending.state == EndCallState::Active {
                            let start = openai::answered_session_start(&state.config, &base_instructions,
                                voice, call.announce_courtside, user_name, opening_spoken, &text);
                            answered_session = Some(AnsweredSessionJob::new(state.config.clone(), start));
                            tracing::info!(%call_id, "real recipient speech confirmed; preparing call objective");
                        }
                        let start_ms = start_ms.saturating_add(transcript_offset_ms);
                        let end_ms = end_ms.saturating_add(transcript_offset_ms);
                        // Recognition is additional evidence for a quiet speaker;
                        // transcript delivery gaps are never treated as silence.
                        silence.recipient_activity(tokio::time::Instant::now());
                        state.broadcast(ServerEvent::TranscriptDelta {
                            call_id,
                            speaker: Speaker::Callee,
                            delta: text.clone(),
                            start_ms,
                            end_ms,
                        });
                        transcript_fragments.push(TranscriptFragment {
                            speaker: Speaker::Callee,
                            text,
                            start_ms,
                            end_ms,
                        });
                        transcript_revision = transcript_revision.saturating_add(1);
                        semantic_failures = 0;
                        if ending.state == EndCallState::Active && !private_question_pending {
                            semantic_recheck_pending = true;
                            semantic_end_timer
                                .as_mut()
                                .reset(tokio::time::Instant::now() + SEMANTIC_END_CHECK_DELAY);
                        }
                    }
                    openai::RealtimeEvent::EndCallRequested { tool_call_id, reason } => {
                        tracing::info!(%call_id, %reason, "call-control agent invoked end_call");
                        let waiting = awaiting_recipient && ending.state == EndCallState::Active;
                        let result = if waiting {
                            openai::end_call_wait_for_recipient_result(&tool_call_id)
                        } else { openai::end_call_tool_result(&tool_call_id) };
                        ai_sink
                            .send(TungsteniteMessage::text(
                                result.to_string(),
                            ))
                            .await?;
                        ai_sink
                            .send(TungsteniteMessage::text(
                                openai::continue_delegated_response().to_string(),
                            ))
                            .await?;
                        if waiting { continue; }
                        state.clear_call_question(call_id).await;
                        recall_job.take();
                        private_question = None;
                        private_question_pending = false;
                        state.clear_call_handoff(call_id, None).await;
                        pending_handoff = None;
                        startup_hello.stop();
                        if let Some(deadline) = ending.begin(tokio::time::Instant::now(), true) {
                            ai_sink
                                .send(TungsteniteMessage::text(
                                    openai::input_audio_mute().to_string(),
                                ))
                                .await?;
                            end_call_timer.as_mut().reset(deadline);
                        }
                    }
                    openai::RealtimeEvent::AskUserRequested { tool_call_id, question } => {
                        tracing::info!(%call_id, "call-control agent requested private user input");
                        let question = question.split_whitespace().collect::<Vec<_>>()
                            .join(" ").chars().take(600).collect::<String>();
                        let already_waiting = private_question.is_some();
                        let mut available = ending.state == EndCallState::Active
                            && call.user_id.is_some() && call.chat_id.is_some()
                            && !question.is_empty() && !state.call_question_timed_out(call_id).await;
                        let mut remembering = false;
                        if available && !already_waiting {
                            let user_id = call.user_id.expect("checked above");
                            let chat_id = call.chat_id.expect("checked above");
                            remembering = crate::db::history_settings::enabled(&state.db, user_id).await.unwrap_or(false);
                            let lookup_note = if remembering { "I’m checking our earlier chats for this detail. " } else { "" };
                            let content = format!(
                                "The other party asked something I need your help with: {question}\n{lookup_note}Reply here within {CALL_QUESTION_WAIT_SECONDS} seconds. I’ll keep them on the line and pass your answer back."
                            );
                            match crate::db::add_assistant_chat_update(
                                &state.db,
                                user_id,
                                chat_id,
                                &content,
                            )
                            .await
                            {
                                Ok(message) => {
                                    private_question = Some(state.set_pending_call_question(call_id, chat_id, &question).await);
                                    private_question_timer.as_mut().reset(private_question.as_ref().expect("just set").deadline);
                                    private_question_checkpoint = CALL_QUESTION_REMINDER_SECONDS;
                                    private_question_pending = true;
                                    silence.reset(tokio::time::Instant::now());
                                    state.broadcast(ServerEvent::ChatMessageAdded { call_id, message });
                                    if remembering {
                                        state.set_call_remembering(call_id, true).await;
                                        let lookup_state = state.clone();
                                        let lookup_call = call.clone();
                                        let lookup_question = question.clone();
                                        let pending = private_question.as_ref().expect("just set");
                                        let question_id = pending.request.id;
                                        let deadline = pending.deadline.min(tokio::time::Instant::now()
                                            + std::time::Duration::from_secs(super::recall::LOOKUP_SECONDS));
                                        recall_job = Some(CallRecallJob {question_id, task:tokio::spawn(async move {
                                            tokio::time::timeout_at(deadline, super::recall::lookup(&lookup_state, &lookup_call, &lookup_question))
                                                .await.map_err(|_| anyhow::anyhow!("history lookup timed out"))?
                                        })});
                                    }
                                }
                                Err(error) => {
                                    available = false;
                                    tracing::error!(?error, %call_id, "failed to publish live call question");
                                }
                            }
                        }
                        let result = if available {
                            openai::ask_user_tool_result(&tool_call_id,
                                private_question.as_ref().map(|pending| pending.remaining_seconds()).unwrap_or(0),
                                already_waiting)
                        } else {
                            openai::ask_user_unavailable_tool_result(&tool_call_id)
                        };
                        ai_sink
                            .send(TungsteniteMessage::text(result.to_string()))
                            .await?;
                        ai_sink
                            .send(TungsteniteMessage::text(
                                openai::continue_ask_user_response().to_string(),
                            ))
                            .await?;
                        if available && !already_waiting {
                            if let Some(pending) = private_question.as_ref() {
                                ai_sink.send(TungsteniteMessage::text(
                                    if remembering { openai::history_lookup_wait_started(pending.remaining_seconds()) }
                                    else { openai::private_question_wait_started(pending.remaining_seconds()) }.to_string(),
                                )).await?;
                            }
                        }
                    }
                    openai::RealtimeEvent::IncomingUpdateRequested { tool_call_id, caller_name, summary, request_handoff } => {
                        let result = if inbound && ending.state == EndCallState::Active {
                            crate::bridge::incoming::update(&state, call_id, &caller_name, &summary, request_handoff).await
                        } else {
                            Err(anyhow::anyhow!("incoming updates are unavailable on this call"))
                        };
                        let status = match result {
                            Ok(request) if request_handoff => {
                                pending_handoff = request;
                                silence.reset(tokio::time::Instant::now());
                                if pending_handoff.is_some() { "waiting_for_owner" } else { "owner_unavailable" }
                            }
                            Ok(_) => "update_posted",
                            Err(error) => {
                                tracing::warn!(?error, %call_id, "incoming caller update failed");
                                "failed"
                            }
                        };
                        ai_sink.send(TungsteniteMessage::text(openai::incoming_update_result(&tool_call_id, status).to_string())).await?;
                        ai_sink.send(TungsteniteMessage::text(openai::continue_delegated_response().to_string())).await?;
                    }
                    openai::RealtimeEvent::SendDtmfRequested { tool_call_id, digits, reason } => {
                        tracing::info!(%call_id, %digits, %reason, "call-control agent requested IVR selection");
                        let invalid_reason = if inbound {
                            Some("Keypad navigation is only available on outbound calls.")
                        } else if ending.state != EndCallState::Active {
                            Some("The call is already closing.")
                        } else if validated_dtmf_digits(&digits).is_none() {
                            Some("The keypad selection must contain one to six characters using only 0-9, star, or pound.")
                        } else {
                            None
                        };
                        if let Some(error) = invalid_reason {
                            ai_sink
                                .send(TungsteniteMessage::text(
                                    openai::send_dtmf_failed_tool_result(&tool_call_id, error).to_string(),
                                ))
                                .await?;
                            ai_sink
                                .send(TungsteniteMessage::text(
                                    openai::continue_delegated_response().to_string(),
                                ))
                                .await?;
                            continue;
                        }

                        let digits = validated_dtmf_digits(&digits).expect("validated above");
                        let twiml = crate::twilio::webhook::dtmf_twiml(&state, call_id, &digits);
                        let action_at_ms = transcript_fragments
                            .iter()
                            .map(|fragment| fragment.end_ms)
                            .max()
                            .unwrap_or_default()
                            .saturating_add(1);
                        // Commit everything heard before the keypress first so a reload keeps
                        // the action between the correct spoken transcript lines.
                        persist_transcript_fragments(
                            &state,
                            call_id,
                            std::mem::take(&mut transcript_fragments),
                        )
                        .await;
                        // Remove this stream's private-input sender before Twilio opens the
                        // replacement stream. Otherwise this stream's cleanup could race and
                        // accidentally remove the new session's sender.
                        state.unregister_live_call_input(call_id).await;
                        match crate::twilio::rest::redirect_call_to_twiml(
                            &state.config,
                            &start.call_sid,
                            &twiml,
                        )
                        .await
                        {
                            Ok(()) => {
                                redirecting_for_dtmf = true;
                                let reason = concise_menu_reason(&reason);
                                let content = if reason.is_empty() {
                                    format!("Pressed {digits}.")
                                } else {
                                    format!("Pressed {digits} for {reason}.")
                                };
                                record_segment(&state, call_id, Speaker::Action, &content).await;
                                state.broadcast(ServerEvent::TranscriptDelta {
                                    call_id,
                                    speaker: Speaker::Action,
                                    delta: content.clone(),
                                    start_ms: action_at_ms,
                                    end_ms: action_at_ms,
                                });
                                break;
                            }
                            Err(error) => {
                                state
                                    .register_live_call_input(call_id, live_input_sender.clone())
                                    .await;
                                tracing::error!(?error, %call_id, "Twilio IVR selection failed");
                                ai_sink
                                    .send(TungsteniteMessage::text(
                                        openai::send_dtmf_failed_tool_result(
                                            &tool_call_id,
                                            "Twilio could not send the keypad selection.",
                                        )
                                        .to_string(),
                                    ))
                                    .await?;
                                ai_sink
                                    .send(TungsteniteMessage::text(
                                        openai::continue_delegated_response().to_string(),
                                    ))
                                    .await?;
                            }
                        }
                    }
                    openai::RealtimeEvent::OutputTranscript { text, start_ms, end_ms } => {
                        if text.trim().is_empty() { continue; }
                        let start_ms = start_ms.saturating_add(transcript_offset_ms);
                        let end_ms = end_ms.saturating_add(transcript_offset_ms);
                        state.broadcast(ServerEvent::TranscriptDelta {
                            call_id,
                            speaker: Speaker::Assistant,
                            delta: text.clone(),
                            start_ms,
                            end_ms,
                        });
                        transcript_fragments.push(TranscriptFragment {
                            speaker: Speaker::Assistant,
                            text,
                            start_ms,
                            end_ms,
                        });
                        transcript_revision = transcript_revision.saturating_add(1);
                        semantic_failures = 0;
                        if ending.state == EndCallState::Active && !private_question_pending {
                            semantic_recheck_pending = true;
                            semantic_end_timer
                                .as_mut()
                                .reset(tokio::time::Instant::now() + SEMANTIC_END_CHECK_DELAY);
                        }
                    }
                    openai::RealtimeEvent::Error(err) => {
                        if err.contains("input_audio_rate_limit_exceeded") {
                            tracing::warn!(%call_id, "Live dropped an audio frame; continuing with paced input");
                            audio_clock_timer.reset_after(AUDIO_CLOCK_INTERVAL);
                            continue;
                        }
                        tracing::error!(%call_id, %err, "openai realtime error");
                        break;
                    }
                    openai::RealtimeEvent::SessionClosed => {
                        live_session_closed = true;
                        break;
                    }
                    openai::RealtimeEvent::InstructionsAppended(event_id) => {
                        if event_id == "courtside_opening" {
                            tracing::info!(%call_id, "live call opening accepted");
                        }
                    }
                    openai::RealtimeEvent::BackendStarted(response_id) => {
                        tracing::info!(%call_id, %response_id, input_buffer_ms=input_clock.queued_ms(), "live backend started");
                        if !response_id.is_empty() { pending_responses.insert(response_id, tokio::time::Instant::now()); }
                    }
                    openai::RealtimeEvent::BackendFinished(response_id) => {
                        if let Some(started) = pending_responses.remove(&response_id) {
                            tracing::info!(%call_id, %response_id, duration_ms=started.elapsed().as_millis(), "live backend completed");
                        }
                        if pending_responses.is_empty() && semantic_recheck_pending {
                            semantic_end_timer.as_mut().reset(tokio::time::Instant::now() + SEMANTIC_END_CHECK_DELAY);
                        }
                    }
                    openai::RealtimeEvent::SessionStarted | openai::RealtimeEvent::Other => {}
                }
            }
            result = wait_for_answered_session(&mut answered_session) => {
                answered_session.take();
                match result {
                    Ok((sink, stream)) => {
                        // The PSTN connection stays open. Only the voice model's
                        // presence-check session is replaced; recorded phone
                        // speech primes the task session, never the internal cue.
                        let _ = ai_sink.send(TungsteniteMessage::text(openai::session_close().to_string())).await;
                        collect_live_close_events(&mut ai_stream, &mut transcript_fragments,
                            &mut startup_hello, true, transcript_offset_ms, &mut replayed_reply).await;
                        let _ = ai_sink.close().await;
                        let heard_reply = transcript_fragments.iter()
                            .filter(|fragment| fragment.speaker == Speaker::Callee)
                            .map(|fragment| fragment.text.as_str()).collect::<Vec<_>>().join("");
                        let audio = reply_audio.take();
                        replayed_reply = ReplayedReply::new(&heard_reply, audio.len());
                        transcript_offset_ms = transcript_fragments.iter()
                            .map(|fragment| fragment.end_ms).max().unwrap_or_default().saturating_add(1)
                            .max(connected_at.elapsed().as_millis().min(i64::MAX as u128) as i64);
                        ai_sink = sink;
                        ai_stream = stream;
                        startup_hello = StartupHello::new(true);
                        pending_responses.clear();
                        input_clock.clear();
                        // Priming must not put every subsequent live frame behind
                        // a replay queue. Only this fresh session receives a
                        // bounded startup burst; ordinary input stays at 20ms.
                        let burst_len = audio.len().min(REPLY_STARTUP_BURST_SAMPLES);
                        if burst_len > 0 {
                            let payload = BASE64.encode(&audio[..burst_len]);
                            ai_sink.send(TungsteniteMessage::text(openai::input_audio_append(&payload).to_string())).await?;
                            startup_hello.record_input(&payload);
                        }
                        input_clock.push_samples(audio[burst_len..].iter().copied());
                        tracing::info!(%call_id, startup_replay_ms=burst_len/8, remaining_replay_ms=input_clock.queued_ms(), "caller reply primed new voice session");
                        awaiting_recipient = false;
                        semantic_recheck_pending = false;
                        transcript_revision = transcript_revision.saturating_add(1);
                        silence.reset(tokio::time::Instant::now());
                        audio_clock_timer.reset_after(AUDIO_CLOCK_INTERVAL);
                        tracing::info!(%call_id, "recipient answered; continuing call objective");
                    }
                    Err(error) => {
                        tracing::error!(?error, %call_id, "could not continue after recipient answered");
                        // The presence check remains usable for a brief goodbye;
                        // a failed model connection must not leave a stuck call.
                        ai_sink.send(TungsteniteMessage::text(openai::call_audio_unavailable_goodbye().to_string())).await?;
                        let deadline = ending.begin(tokio::time::Instant::now(), true).expect("active call");
                        startup_hello.stop();
                        ai_sink.send(TungsteniteMessage::text(openai::input_audio_mute().to_string())).await?;
                        end_call_timer.as_mut().reset(deadline);
                    }
                }
            }
            Some((checked_revision, result)) = semantic_result_receiver.recv() => {
                semantic_check_in_flight = false;
                match result {
                    Ok(Some(reason))
                        if checked_revision == transcript_revision
                            && ending.state == EndCallState::Active
                            && !private_question_pending
                            && pending_handoff.is_none()
                            && !awaiting_recipient
                            && pending_responses.is_empty()
                            && !startup_hello.active() =>
                    {
                        tracing::info!(
                            %call_id,
                            %reason,
                            "semantic completion watchdog is ending the call"
                        );
                        let deadline = ending.begin(tokio::time::Instant::now(), false)
                            .expect("active call");
                        startup_hello.stop();
                        ai_sink
                            .send(TungsteniteMessage::text(
                                openai::input_audio_mute().to_string(),
                            ))
                            .await?;
                        // The conversation has already reached a natural closing. Place the
                        // marker behind any audio Twilio still has buffered, then disconnect.
                        end_call_timer.as_mut().reset(deadline);
                    }
                    Ok(Some(_)) if ending.state == EndCallState::Active => {
                        semantic_recheck_pending = true;
                    }
                    Ok(Some(_)) | Ok(None) => {}
                    Err(error) => {
                        tracing::warn!(%call_id, %error, "semantic completion watchdog failed");
                        semantic_failures = semantic_failures.saturating_add(1);
                        if semantic_failures < 3 {
                            semantic_recheck_pending = true;
                        }
                    }
                }
                if semantic_recheck_pending
                    && ending.state == EndCallState::Active
                    && !private_question_pending
                    && pending_handoff.is_none()
                    && !awaiting_recipient
                {
                    let delay = if semantic_failures == 0 {
                        std::time::Duration::from_millis(350)
                    } else {
                        std::time::Duration::from_secs(3)
                    };
                    semantic_end_timer.as_mut().reset(tokio::time::Instant::now() + delay);
                }
            }
            _ = &mut semantic_end_timer,
                if ending.state == EndCallState::Active
                    && !semantic_check_in_flight
                    && !private_question_pending
                    && pending_handoff.is_none()
                    && !awaiting_recipient
                    && pending_responses.is_empty()
                    && !startup_hello.active()
                    && semantic_recheck_pending =>
            {
                semantic_recheck_pending = false;
                semantic_end_timer.as_mut().reset(
                    tokio::time::Instant::now() + std::time::Duration::from_secs(86_400),
                );
                if let Some(transcript) = semantic_transcript_preview(&transcript_fragments) {
                    semantic_check_in_flight = true;
                    let checked_revision = transcript_revision;
                    let config = state.config.clone();
                    let task = call.task_prompt.clone();
                    let sender = semantic_result_sender.clone();
                    tokio::spawn(async move {
                        let result = openai::assess_call_completion(
                            &config,
                            task.as_deref(),
                            &transcript,
                        )
                        .await
                        .map_err(|error| error.to_string());
                        let _ = sender.send((checked_revision, result));
                    });
                }
            }
            _ = &mut opening_timer,
                if ending.state == EndCallState::Active && !opening_spoken
                    && !recipient_spoken && startup_hello.can_start() =>
            {
                if !private_question_pending && pending_responses.is_empty() {
                    // This is caller input for Live, never phone playback. The
                    // configured Live voice generates the entire real opening.
                    tracing::info!(%call_id, "quiet Live startup; inserting input-only hello");
                    input_clock.clear();
                    startup_hello.start();
                    opening_audio_timer.reset();
                }
                opening_timer.as_mut().reset(tokio::time::Instant::now() + std::time::Duration::from_secs(2));
            }
            _ = opening_audio_timer.tick(),
                if ending.state == EndCallState::Active && startup_hello.active() =>
            {
                if let Some(audio) = startup_hello.next_frame() {
                    let payload = BASE64.encode(audio);
                    ai_sink.send(TungsteniteMessage::text(openai::input_audio_append(&payload).to_string())).await?;
                    audio_clock_timer.reset_after(AUDIO_CLOCK_INTERVAL);
                }
            }
            _ = audio_clock_timer.tick(), if ending.state != EndCallState::WaitingForPlayback && !startup_hello.active() => {
                // Continue the timeline during the goodbye, using only silence.
                // Input is muted and the recipient cannot reopen a finished call.
                let payload = input_clock.frame();
                ai_sink.send(TungsteniteMessage::text(openai::input_audio_append(&payload).to_string())).await?;
                startup_hello.record_input(&payload);
            }
            _ = private_question_timer.as_mut(), if private_question_pending && ending.state == EndCallState::Active => {
                // Expire at the absolute deadline, independently of the one-second
                // silence watchdog. A queued answer can still win this race.
                private_question_timer.as_mut().reset(tokio::time::Instant::now() + std::time::Duration::from_secs(86_400));
                if let Some(pending) = private_question.as_ref() {
                    if state.expire_call_question(call_id, pending.request.id).await {
                        let chat_id = pending.request.chat_id;
                        let expired_question = pending.request.question.clone();
                        recall_job.take();
                        private_question = None;
                        private_question_pending = false;
                        pending_responses.clear();
                        silence.reset(tokio::time::Instant::now());
                        ai_sink.send(TungsteniteMessage::text(openai::backend_question_timed_out(&expired_question).to_string())).await?;
                        ai_sink.send(TungsteniteMessage::text(openai::private_question_timed_out(inbound).to_string())).await?;
                        ai_sink.send(TungsteniteMessage::text(openai::private_question_timeout_speech(inbound).to_string())).await?;
                        tracing::info!(%call_id, "private question expired; spoken update requested");
                        if let Some(user_id) = call.user_id {
                            let content = format!("The {CALL_QUESTION_WAIT_SECONDS}-second wait ended without an answer. Courtside will continue without that detail if possible, or take a message and wrap up.");
                            match crate::db::add_assistant_chat_update(&state.db, user_id, chat_id, &content).await {
                                Ok(message) => state.broadcast(ServerEvent::ChatMessageAdded { call_id, message }),
                                Err(error) => tracing::warn!(?error, %call_id, "failed to save private question timeout"),
                            }
                        }
                    }
                }
            }
            _ = silence_timer.tick(), if ending.state == EndCallState::Active => {
                pending_responses.retain(|_, started| started.elapsed() < std::time::Duration::from_secs(10));
                if let Some(pending) = private_question.as_ref() {
                    let remaining = pending.remaining_seconds();
                    if remaining > 0 && private_question_checkpoint > 0 && remaining <= private_question_checkpoint
                        && state.call_is_waiting_for_user(call_id).await {
                        ai_sink.send(TungsteniteMessage::text(openai::private_question_countdown(remaining).to_string())).await?;
                        private_question_checkpoint = 0;
                    }
                }
                if let Some(request_id) = pending_handoff.as_ref()
                    .filter(|request| request.expires_at <= chrono::Utc::now())
                    .map(|request| request.id)
                {
                    if state.clear_call_handoff(call_id, Some(request_id)).await {
                        ai_sink.send(TungsteniteMessage::text(openai::incoming_handoff_unavailable().to_string())).await?;
                        crate::bridge::incoming::unavailable(&state, call_id).await;
                    }
                    pending_handoff = None;
                    silence.reset(tokio::time::Instant::now());
                }
                let pending_work = private_question_pending || !pending_responses.is_empty()
                    || pending_handoff.is_some() || startup_hello.active() || answered_session.is_some();
                match silence.check(tokio::time::Instant::now(), pending_work) {
                    Some(SilenceAction::CheckIn) => {
                        tracing::info!(%call_id, "checking for a reply on a silent line");
                        ai_sink.send(TungsteniteMessage::text(openai::quiet_line_check_in().to_string())).await?;
                    }
                    Some(SilenceAction::End) => {
                        tracing::info!(%call_id, "no reply to silent-line check; ending call");
                        ai_sink.send(TungsteniteMessage::text(openai::quiet_line_goodbye().to_string())).await?;
                        let deadline = ending.begin(tokio::time::Instant::now(), true).expect("active call");
                        startup_hello.stop();
                        ai_sink.send(TungsteniteMessage::text(openai::input_audio_mute().to_string())).await?;
                        end_call_timer.as_mut().reset(deadline);
                    }
                    None => {}
                }
            }
            _ = &mut end_call_timer, if ending.state != EndCallState::Active => {
                match ending.state {
                    EndCallState::Closing => {
                        let marker = twilio_ws::TwilioOutbound::mark(&stream_sid, END_CALL_MARK);
                        socket
                            .send(AxumMessage::text(serde_json::to_string(&marker)?))
                            .await?;
                        let deadline = ending.wait_for_playback(tokio::time::Instant::now());
                        end_call_timer.as_mut().reset(deadline);
                    }
                    EndCallState::WaitingForPlayback => {
                        tracing::warn!(%call_id, "closing playback marker timed out; forcing hangup");
                        if let Err(err) = crate::twilio::rest::hangup_call(&state.config, &start.call_sid).await {
                            tracing::warn!(?err, call_sid = %start.call_sid, "agent end_call fallback failed");
                        }
                        break;
                    }
                    EndCallState::Active => {}
                }
            }
        }
    }

    tracing::info!(%call_id, "media stream ended");
    answered_session.take();
    recall_job.take();
    state.set_call_remembering(call_id, false).await;
    let handed_off = matches!(
        state.call_mode(call_id).await,
        CallMode::TakeoverPending | CallMode::Takeover
    );
    if !redirecting_for_dtmf {
        state.unregister_live_call_input(call_id).await;
    }
    if !live_session_closed {
        let _ = ai_sink
            .send(TungsteniteMessage::text(
                openai::session_close().to_string(),
            ))
            .await;
        collect_live_close_events(
            &mut ai_stream,
            &mut transcript_fragments,
            &mut startup_hello,
            real_recipient_audio || !awaiting_recipient,
            transcript_offset_ms,
            &mut replayed_reply,
        )
        .await;
    }
    let _ = ai_sink.close().await;
    persist_transcript_fragments(&state, call_id, transcript_fragments).await;
    if redirecting_for_dtmf {
        tracing::info!(%call_id, "assistant stream redirected for IVR keypad selection");
        return Ok(());
    }
    if handed_off
        || matches!(
            state.call_mode(call_id).await,
            CallMode::TakeoverPending | CallMode::Takeover
        )
    {
        tracing::info!(%call_id, "AI bridge handed off to the user; keeping call open");
        return Ok(());
    }
    // Outbound usage is finalized by Twilio's status callback. Incoming calls
    // do not have that callback, so use the connected media-stream duration.
    if inbound {
        let seconds = connected_at.elapsed().as_secs().min(i64::MAX as u64) as i64;
        if let Err(err) = crate::db::finalize_call_usage(&state.db, call_id, seconds).await {
            tracing::error!(?err, %call_id, "failed to finalize inbound usage");
        }
        state
            .set_status_and_broadcast(call_id, CallStatus::Completed)
            .await;
    }

    // Summarize off the hot path; the sockets are already closed.
    let state_for_summary = state.clone();
    tokio::spawn(async move {
        if let Err(err) = summarize_call(&state_for_summary, call_id).await {
            tracing::error!(?err, %call_id, "failed to summarize call");
        }
    });

    Ok(())
}

/// Remove a relay even if an early socket error exits the bridge through `?`.
struct VoiceRegistration {
    state: AppState,
    call_id: Uuid,
    bridge: Uuid,
}
impl Drop for VoiceRegistration {
    fn drop(&mut self) {
        let state = self.state.clone();
        let call_id = self.call_id;
        let bridge = self.bridge;
        tokio::spawn(async move {
            state.unregister_voice_relay(call_id, bridge).await;
            state.set_call_remembering(call_id, false).await;
        });
    }
}

enum NativeExit {
    Return(Option<oneshot::Sender<VoiceResult>>),
    CallEnded,
}

async fn native_takeover(
    socket: &mut WebSocket,
    commands: &mut mpsc::Receiver<VoiceCommand>,
    state: &AppState,
    call_id: Uuid,
    stream_sid: &str,
    peer: VoicePeer,
) -> anyhow::Result<NativeExit> {
    let (callee_audio, callee_observer) =
        spawn_audio_observer(state.clone(), call_id, Speaker::Callee);
    let (user_audio, user_observer) = spawn_audio_observer(state.clone(), call_id, Speaker::User);
    let observers = (callee_audio, user_audio);
    let result = native_audio_loop(
        socket, commands, state, call_id, stream_sid, &peer, &observers,
    )
    .await;
    let ended = matches!(result, Ok(NativeExit::CallEnded) | Err(_));
    let _ = peer.output.try_send(VoiceOutput::Ended(
        if ended {
            "The call ended."
        } else {
            "Returning the call to Courtside…"
        }
        .into(),
    ));
    drop(observers);
    // Flush final words before restoring the assistant's conversation context.
    tokio::join!(
        finish_observer(callee_observer),
        finish_observer(user_observer)
    );
    result
}

async fn native_audio_loop(
    socket: &mut WebSocket,
    commands: &mut mpsc::Receiver<VoiceCommand>,
    state: &AppState,
    call_id: Uuid,
    stream_sid: &str,
    peer: &VoicePeer,
    observers: &(mpsc::Sender<String>, mpsc::Sender<String>),
) -> anyhow::Result<NativeExit> {
    // The agent's tools and completion timers are not polled in this loop.
    let result = loop {
        tokio::select! {
            _ = peer.output.closed() => break Ok(NativeExit::Return(None)),
            message = socket.recv() => {
                match message {
                    Some(Ok(AxumMessage::Text(text))) => match serde_json::from_str::<twilio_ws::TwilioInbound>(&text) {
                        Ok(twilio_ws::TwilioInbound::Media { media }) => {
                            if let Ok(bytes) = BASE64.decode(&media.payload) {
                                // Only Twilio's inbound (callee) stream reaches
                                // the app's speaker. Never echo its microphone.
                                let _ = peer.output.try_send(VoiceOutput::Audio(bytes));
                                state.broadcast_audio(call_id, AudioTrack::Callee, &media.payload);
                                let _ = observers.0.try_send(media.payload);
                            }
                        },
                        Ok(twilio_ws::TwilioInbound::Stop {}) => break Ok(NativeExit::CallEnded),
                        _ => {},
                    },
                    Some(Ok(AxumMessage::Close(_))) | None => break Ok(NativeExit::CallEnded),
                    Some(Err(error)) => break Err(error.into()),
                    _ => {},
                }
            }
            command = commands.recv() => match command {
                Some(VoiceCommand::Microphone { session, audio }) if session == peer.session => {
                    let payload = BASE64.encode(audio);
                    let frame = twilio_ws::TwilioOutbound::media(stream_sid, payload.clone());
                    if let Err(error) = socket.send(AxumMessage::text(serde_json::to_string(&frame)?)).await {
                        break Err(error.into());
                    }
                    state.broadcast_audio(call_id, AudioTrack::User, &payload);
                    let _ = observers.1.try_send(payload);
                },
                Some(VoiceCommand::Leave { session, reply }) if owns_session(peer.session, session) => {
                    break Ok(NativeExit::Return(reply));
                },
                Some(VoiceCommand::Join { reply, .. }) => {
                    let _ = reply.send(Err("You are already joined to this call.".into()));
                },
                Some(VoiceCommand::Leave { reply: Some(reply), .. }) => { let _ = reply.send(Ok(())); },
                Some(VoiceCommand::DeclineHandoff { reply, .. }) => {
                    let _ = reply.send(Err("You are already joined to this call.".into()));
                },
                Some(_) => {},
                None => break Ok(NativeExit::CallEnded),
            }
        }
    };
    result
}

enum RestoreExit {
    Ready(openai::OpenAiSink, openai::OpenAiStream),
    Joined(VoicePeer),
    CallEnded,
}

/// Keep receiving Twilio media while opening a fresh AI session. A temporary
/// model/network failure must not hang up the person still on the phone.
async fn restore_assistant(
    socket: &mut WebSocket,
    commands: &mut mpsc::Receiver<VoiceCommand>,
    state: &AppState,
    call_id: Uuid,
    stream_sid: &str,
    base_instructions: &str,
    voice: shared::CallVoice,
    inbound: bool,
    replies: &mut Vec<oneshot::Sender<VoiceResult>>,
) -> anyhow::Result<RestoreExit> {
    state
        .set_call_control(
            call_id,
            CallMode::Assistant,
            "Returning the call to Courtside…",
        )
        .await;
    let mut instructions = base_instructions.to_string();
    match transcript_context(state, call_id).await {
        Ok(prior) => {
            instructions.push_str("\n\nConversation before the user handed this call back:\n");
            instructions.push_str(&prior);
        }
        Err(error) => tracing::warn!(?error, %call_id, "failed to load takeover context"),
    }
    let clear = twilio_ws::TwilioOutbound::clear(stream_sid);
    socket
        .send(AxumMessage::text(serde_json::to_string(&clear)?))
        .await?;
    state.broadcast_audio(call_id, AudioTrack::User, "");
    let reconnect = async {
        loop {
            let attempt = tokio::time::timeout(std::time::Duration::from_secs(15), async {
                let (mut sink, mut stream) = openai::connect(&state.config).await?;
                sink.send(TungsteniteMessage::text(
                    openai::call_session_start(&state.config, &instructions, voice, inbound)
                        .to_string(),
                ))
                .await?;
                wait_for_live_start(&mut stream).await?;
                sink.send(TungsteniteMessage::text(
                    openai::resume_request().to_string(),
                ))
                .await?;
                Ok::<_, anyhow::Error>((sink, stream))
            })
            .await;
            match attempt {
                Ok(Ok(session)) => break session,
                _ => {
                    tracing::warn!(%call_id, "assistant reconnection failed; retaining the phone call and retrying");
                    state.set_call_control(call_id, CallMode::Assistant,
                        "Courtside is reconnecting to the call. You can take over again while it retries.").await;
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                }
            }
        }
    };
    tokio::pin!(reconnect);
    loop {
        tokio::select! {
            (sink, stream) = &mut reconnect => {
                let lock = state.call_control_lock(call_id).await;
                let _guard = lock.lock().await;
                if state.call_mode(call_id).await != CallMode::Assistant {
                    return Ok(RestoreExit::CallEnded);
                }
                state.set_call_control(call_id, CallMode::Assistant, "Courtside has the call again.").await;
                for reply in replies.drain(..) { let _ = reply.send(Ok(())); }
                return Ok(RestoreExit::Ready(sink, stream));
            },
            message = socket.recv() => match message {
                Some(Ok(AxumMessage::Text(text))) => {
                    if matches!(serde_json::from_str::<twilio_ws::TwilioInbound>(&text), Ok(twilio_ws::TwilioInbound::Stop {})) {
                        return Ok(RestoreExit::CallEnded);
                    }
                },
                Some(Ok(AxumMessage::Close(_))) | None => return Ok(RestoreExit::CallEnded),
                Some(Err(error)) => return Err(error.into()),
                _ => {},
            },
            command = commands.recv() => match command {
                Some(VoiceCommand::Join { session, output, reply }) if !reply.is_closed() && !output.is_closed() => {
                    let lock = state.call_control_lock(call_id).await;
                    let _guard = lock.lock().await;
                    if state.call_mode(call_id).await != CallMode::Assistant {
                        let _ = reply.send(Err("Someone is already taking over this call.".into()));
                        continue;
                    }
                    state.set_call_control(call_id, CallMode::InAppTakeover,
                        "You’re on the call. Use Return to AI when you’re done.").await;
                    let _ = reply.send(Ok(()));
                    for reply in replies.drain(..) { let _ = reply.send(Ok(())); }
                    return Ok(RestoreExit::Joined(VoicePeer { session, output }));
                },
                Some(VoiceCommand::Leave { reply: Some(reply), .. }) => replies.push(reply),
                Some(VoiceCommand::DeclineHandoff { reply, .. }) => {
                    let _ = reply.send(Err("That invitation is no longer pending.".into()));
                },
                Some(_) => {},
                None => return Ok(RestoreExit::CallEnded),
            }
        }
    }
}

fn spawn_audio_observer(
    state: AppState,
    call_id: Uuid,
    speaker: Speaker,
) -> (mpsc::Sender<String>, tokio::task::JoinHandle<()>) {
    let (audio, receiver) = mpsc::channel(250);
    let task = tokio::spawn(async move {
        if let Err(error) = audio_observer(state, call_id, speaker, receiver).await {
            tracing::warn!(?error, %call_id, ?speaker, "takeover transcription unavailable");
        }
    });
    (audio, task)
}

async fn finish_observer(mut task: tokio::task::JoinHandle<()>) {
    if tokio::time::timeout(std::time::Duration::from_secs(3), &mut task)
        .await
        .is_err()
    {
        task.abort();
    }
}

/// One observer per source keeps the two microphone tracks and their speaker
/// labels accurate even when both people speak at once.
async fn audio_observer(
    state: AppState,
    call_id: Uuid,
    speaker: Speaker,
    mut audio: mpsc::Receiver<String>,
) -> anyhow::Result<()> {
    let call = crate::db::find_call(&state.db, call_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("unknown call"))?;
    let mut instructions = "Silently transcribe one person's side of a phone call. Never speak or take any actions. Retain facts and decisions for the assistant when it resumes.".to_string();
    instructions.push_str(&format!("\nThis microphone belongs to: {speaker:?}.\n"));
    if let Some(task) = call.task_prompt {
        instructions.push_str(&task);
    }
    let (mut sink, mut stream) = openai::connect(&state.config).await?;
    sink.send(TungsteniteMessage::text(
        openai::observer_session_start(&state.config, &instructions, shared::CallVoice::default())
            .to_string(),
    ))
    .await?;
    wait_for_live_start(&mut stream).await?;
    let mut fragments = Vec::new();
    let mut closed = false;
    loop {
        tokio::select! {
            frame = audio.recv() => {
                let Some(frame) = frame else { break };
                if sink.send(TungsteniteMessage::text(openai::input_audio_append(&frame).to_string())).await.is_err() { break; }
            },
            event = stream.next() => {
                let Some(Ok(TungsteniteMessage::Text(text))) = event else { break };
                match openai::parse_event(&text) {
                    openai::RealtimeEvent::InputTranscript { text, start_ms, end_ms } => {
                        state.append_takeover_context(call_id, speaker, &text).await;
                        state.broadcast(ServerEvent::TranscriptDelta { call_id, speaker, delta: text.clone(), start_ms, end_ms });
                        fragments.push(TranscriptFragment { speaker, text, start_ms, end_ms });
                    },
                    openai::RealtimeEvent::SessionClosed => { closed = true; break; },
                    openai::RealtimeEvent::Error(_) => break,
                    _ => {},
                }
            }
        }
    }
    if !closed {
        let _ = sink
            .send(TungsteniteMessage::text(
                openai::session_close().to_string(),
            ))
            .await;
        collect_observer_close_events(&mut stream, &mut fragments, speaker).await;
    }
    let _ = sink.close().await;
    persist_transcript_fragments(&state, call_id, fragments).await;
    Ok(())
}

async fn observe_run(mut socket: WebSocket, state: AppState) -> anyhow::Result<()> {
    let start = loop {
        let Some(message) = socket.recv().await else {
            anyhow::bail!("takeover stream closed before start frame");
        };
        if let AxumMessage::Text(text) = message? {
            match serde_json::from_str::<twilio_ws::TwilioInbound>(&text) {
                Ok(twilio_ws::TwilioInbound::Start { start }) => break start,
                Ok(_) => continue,
                Err(err) => tracing::warn!(?err, "unrecognized takeover frame before start"),
            }
        }
    };
    let call_id: Uuid = start
        .custom_parameters
        .get("call_id")
        .and_then(|raw| raw.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("takeover start frame missing call_id"))?;
    let call = crate::db::find_call(&state.db, call_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("unknown takeover call {call_id}"))?;
    let prior = transcript_context(&state, call_id).await?;
    let mut instructions = String::from(
        "You are silently observing a live phone call after the Courtside user took over. \
         Listen carefully and retain facts, decisions, commitments, and unresolved questions. \
         Never speak. The transcript is used if the user hands the call back to the assistant.",
    );
    if !prior.is_empty() {
        instructions.push_str("\n\nConversation before takeover:\n");
        instructions.push_str(&prior);
    }
    if let Some(task) = call.task_prompt.as_deref() {
        instructions.push_str("\n\nOriginal call objective:\n");
        instructions.push_str(task);
    }
    let voice = match call.user_id {
        Some(user_id) => crate::db::find_user_by_id(&state.db, user_id)
            .await?
            .as_ref()
            .map(|user| crate::db::call_preferences(user).voice)
            .unwrap_or_default(),
        None => shared::CallVoice::default(),
    };

    let (mut ai_sink, mut ai_stream) = openai::connect(&state.config).await?;
    ai_sink
        .send(TungsteniteMessage::text(
            openai::observer_session_start(&state.config, &instructions, voice).to_string(),
        ))
        .await?;
    wait_for_live_start(&mut ai_stream).await?;
    let mut fragments = Vec::new();
    let mut latest_speaker = Speaker::Callee;
    let mut session_closed = false;
    tracing::info!(%call_id, stream_sid = %start.stream_sid, "takeover observer started");

    loop {
        tokio::select! {
            twilio_message = socket.recv() => {
                let Some(twilio_message) = twilio_message else { break };
                let AxumMessage::Text(text) = twilio_message? else { continue };
                match serde_json::from_str::<twilio_ws::TwilioInbound>(&text) {
                    Ok(twilio_ws::TwilioInbound::Media { media }) => {
                        latest_speaker = if media.track.as_deref() == Some("outbound") {
                            Speaker::User
                        } else {
                            Speaker::Callee
                        };
                        let track = if latest_speaker == Speaker::User {
                            AudioTrack::User
                        } else {
                            AudioTrack::Callee
                        };
                        state.broadcast_audio(call_id, track, &media.payload);
                        ai_sink
                            .send(TungsteniteMessage::text(
                                openai::input_audio_append(&media.payload).to_string(),
                            ))
                            .await?;
                    }
                    Ok(twilio_ws::TwilioInbound::Stop {}) => break,
                    Ok(_) => {}
                    Err(err) => tracing::warn!(?err, %call_id, "unrecognized takeover frame"),
                }
            }
            ai_message = ai_stream.next() => {
                let Some(ai_message) = ai_message else { break };
                let TungsteniteMessage::Text(text) = ai_message? else { continue };
                match openai::parse_event(&text) {
                    openai::RealtimeEvent::InputTranscript { text, start_ms, end_ms } => {
                        state
                            .append_takeover_context(call_id, latest_speaker, &text)
                            .await;
                        state.broadcast(ServerEvent::TranscriptDelta {
                            call_id,
                            speaker: latest_speaker,
                            delta: text.clone(),
                            start_ms,
                            end_ms,
                        });
                        fragments.push(TranscriptFragment {
                            speaker: latest_speaker,
                            text,
                            start_ms,
                            end_ms,
                        });
                    }
                    openai::RealtimeEvent::Error(err) => {
                        tracing::error!(%call_id, %err, "takeover observer Live error");
                        break;
                    }
                    openai::RealtimeEvent::SessionClosed => {
                        session_closed = true;
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
    if !session_closed {
        let _ = ai_sink
            .send(TungsteniteMessage::text(
                openai::session_close().to_string(),
            ))
            .await;
        collect_observer_close_events(&mut ai_stream, &mut fragments, latest_speaker).await;
    }
    let _ = ai_sink.close().await;
    persist_transcript_fragments(&state, call_id, fragments).await;
    tracing::info!(%call_id, "takeover observer stopped");
    Ok(())
}

async fn collect_observer_close_events(
    ai_stream: &mut openai::OpenAiStream,
    fragments: &mut Vec<TranscriptFragment>,
    speaker: Speaker,
) {
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while let Some(Ok(TungsteniteMessage::Text(text))) = ai_stream.next().await {
            match openai::parse_event(&text) {
                openai::RealtimeEvent::InputTranscript {
                    text,
                    start_ms,
                    end_ms,
                } => fragments.push(TranscriptFragment {
                    speaker,
                    text,
                    start_ms,
                    end_ms,
                }),
                openai::RealtimeEvent::SessionClosed => break,
                _ => {}
            }
        }
    })
    .await;
}

async fn wait_for_live_start(ai_stream: &mut openai::OpenAiStream) -> anyhow::Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while let Some(message) = ai_stream.next().await {
            let TungsteniteMessage::Text(text) = message? else {
                continue;
            };
            match openai::parse_event(&text) {
                openai::RealtimeEvent::SessionStarted => return Ok(()),
                openai::RealtimeEvent::Error(err) => {
                    anyhow::bail!("GPT-Live session start failed: {err}")
                }
                _ => {}
            }
        }
        anyhow::bail!("GPT-Live closed before session.started")
    })
    .await
    .map_err(|_| anyhow::anyhow!("timed out waiting for GPT-Live session.started"))?
}

async fn collect_live_close_events(
    ai_stream: &mut openai::OpenAiStream,
    fragments: &mut Vec<TranscriptFragment>,
    startup_hello: &mut StartupHello,
    allow_callee_transcript: bool,
    transcript_offset_ms: i64,
    replayed_reply: &mut ReplayedReply,
) {
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while let Some(Ok(TungsteniteMessage::Text(text))) = ai_stream.next().await {
            match openai::parse_event(&text) {
                openai::RealtimeEvent::InputTranscript {
                    text,
                    start_ms,
                    end_ms,
                } if allow_callee_transcript
                    && !startup_hello.discard_transcript(&text, start_ms, end_ms) =>
                {
                    if let Some(text) = replayed_reply.filter(&text, end_ms) {
                        fragments.push(TranscriptFragment {
                            speaker: Speaker::Callee,
                            text,
                            start_ms: start_ms.saturating_add(transcript_offset_ms),
                            end_ms: end_ms.saturating_add(transcript_offset_ms),
                        });
                    }
                }
                openai::RealtimeEvent::OutputTranscript {
                    text,
                    start_ms,
                    end_ms,
                } => fragments.push(TranscriptFragment {
                    speaker: Speaker::Assistant,
                    text,
                    start_ms: start_ms.saturating_add(transcript_offset_ms),
                    end_ms: end_ms.saturating_add(transcript_offset_ms),
                }),
                openai::RealtimeEvent::SessionClosed => break,
                _ => {}
            }
        }
    })
    .await;
}

async fn persist_transcript_fragments(
    state: &AppState,
    call_id: Uuid,
    mut fragments: Vec<TranscriptFragment>,
) {
    fragments.sort_by_key(|fragment| fragment.start_ms);
    let mut groups: Vec<TranscriptFragment> = Vec::new();
    for fragment in fragments {
        if fragment.text.is_empty() {
            continue;
        }
        if let Some(last) = groups.last_mut() {
            if last.speaker == fragment.speaker && fragment.start_ms <= last.end_ms + 1_200 {
                last.text.push_str(&fragment.text);
                last.end_ms = last.end_ms.max(fragment.end_ms);
                continue;
            }
        }
        groups.push(fragment);
    }
    for group in groups {
        record_segment(state, call_id, group.speaker, &group.text).await;
    }
}

async fn record_segment(state: &AppState, call_id: Uuid, speaker: Speaker, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    match crate::db::add_segment(&state.db, call_id, speaker, text).await {
        Ok(segment) => state.broadcast(ServerEvent::Transcript { segment }),
        Err(err) => tracing::error!(?err, %call_id, "failed to persist transcript segment"),
    }
}

pub(crate) async fn summarize_call(state: &AppState, call_id: Uuid) -> anyhow::Result<()> {
    if !state.begin_summary(call_id).await {
        return Ok(());
    }
    // Twilio's terminal callback can arrive before the media socket has
    // flushed its final transcription. Let the bridge finish first, including
    // both microphone observers used by in-app takeover.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(4), async {
        while state.voice_relay(call_id).await.is_some() {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await;
    let call = crate::db::find_call(&state.db, call_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("unknown call {call_id}"))?;
    let transcript = transcript_context(state, call_id).await?;

    let summary = if transcript.is_empty() {
        "The call ended without enough audible conversation to produce a result.".to_string()
    } else {
        match openai::summarize_transcript(&state.config, &transcript).await {
            Ok(summary) => summary,
            Err(err) => {
                tracing::warn!(?err, %call_id, "summary model failed; sending fallback update");
                "The call ended, but I couldn't generate its summary. Open Calls to review its status."
                    .to_string()
            }
        }
    };
    crate::db::set_call_summary(&state.db, call_id, &summary).await?;
    state.broadcast(ServerEvent::SummaryReady {
        call_id,
        summary: summary.clone(),
    });

    if let (Some(user_id), Some(chat_id)) = (call.user_id, call.chat_id) {
        let content = format!("Call update for {}: {summary}", call.peer_number);
        let message =
            crate::db::add_assistant_chat_update(&state.db, user_id, chat_id, &content).await?;
        state.broadcast(ServerEvent::ChatMessageAdded { call_id, message });
    }
    state.clear_takeover_context(call_id).await;
    Ok(())
}

async fn transcript_context(state: &AppState, call_id: Uuid) -> anyhow::Result<String> {
    let segments = crate::db::call_transcript(&state.db, call_id).await?;
    let persisted = segments
        .into_iter()
        .map(|segment| {
            let speaker = match segment.speaker {
                Speaker::Assistant => "Courtside",
                Speaker::Callee => "Other party",
                Speaker::User => "User",
                Speaker::Action => "Call action",
            };
            format!("{speaker}: {}", segment.text)
        })
        .collect::<Vec<_>>()
        .join("\n");
    let live_handoff = state.takeover_context(call_id).await;
    Ok(match (persisted.is_empty(), live_handoff.is_empty()) {
        (true, true) => String::new(),
        (false, true) => persisted,
        (true, false) => live_handoff,
        (false, false) => format!("{persisted}\n{live_handoff}"),
    })
}

#[cfg(test)]
#[path = "call_bridge_tests.rs"]
mod integration_tests;

#[cfg(test)]
mod tests {
    use super::{
        concise_menu_reason, semantic_transcript_preview, validated_dtmf_digits, TranscriptFragment,
    };

    #[test]
    fn completion_can_assess_a_delivered_message_without_callee_speech() {
        let fragments = [TranscriptFragment {
            speaker: shared::Speaker::Assistant,
            text: "Happy birthday!".into(),
            start_ms: 0,
            end_ms: 1000,
        }];
        assert_eq!(
            semantic_transcript_preview(&fragments).as_deref(),
            Some("Courtside: Happy birthday!")
        );
    }

    #[test]
    fn keypad_selection_accepts_only_short_dtmf_sequences() {
        assert_eq!(validated_dtmf_digits(" 1 ").as_deref(), Some("1"));
        assert_eq!(validated_dtmf_digits("*0#").as_deref(), Some("*0#"));
        assert!(validated_dtmf_digits("").is_none());
        assert!(validated_dtmf_digits("12w3").is_none());
        assert!(validated_dtmf_digits("1234567").is_none());
        assert!(validated_dtmf_digits("1\"/><Hangup/>").is_none());
    }

    #[test]
    fn menu_reason_is_flat_and_bounded_for_chat_updates() {
        assert_eq!(
            concise_menu_reason("  Hospital\n scheduling  "),
            "Hospital scheduling"
        );
        assert!(concise_menu_reason(&"x".repeat(200)).chars().count() <= 80);
    }
}
