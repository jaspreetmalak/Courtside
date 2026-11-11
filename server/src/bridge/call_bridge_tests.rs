//! Entire bridge exercised using local model, Twilio socket, and REST fixtures.
//! These tests never call OpenAI or Twilio or dial a real phone.

use axum::{
    extract::ws::{Message, WebSocketUpgrade},
    routing::{any, post},
    Form, Json, Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::{collections::HashMap, time::Duration};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message as ClientMessage};
use uuid::Uuid;

use crate::{config::Config, state::AppState};

#[derive(Clone, Copy, PartialEq)]
enum Scenario {
    Opening,
    RateLimitedOpening,
    InputHello,
    CallerFirst,
    CallerInterruptsCue,
    DelayedHelloTranscript,
    ReplyAfterCue,
    ReplyWhileConnecting,
    TakeoverWhileConnecting,
    TaskConnectionFails,
    MismatchedSyntheticCaption,
    PrematureCompletion,
    SilentClosing,
    ContinuousClosing,
    SemanticClosing,
    IncomingHandoff,
    IncomingMessage,
    IncomingQuestion,
    RecallFound,
    RecallInvalid,
    RecallSlow,
}

struct Fixture {
    state: AppState,
    call_id: Uuid,
    address: std::net::SocketAddr,
    resumed: bool,
    hangups: mpsc::UnboundedReceiver<HashMap<String, String>>,
    model_commands: mpsc::UnboundedReceiver<Value>,
    app: tokio::task::JoinHandle<()>,
    model: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.app.abort();
        self.model.abort();
    }
}

async fn fixture(scenario: Scenario) -> Fixture {
    fixture_with_config(scenario, None, shared::CallVoice::default()).await
}

async fn fixture_with_config(
    scenario: Scenario,
    config_override: Option<Config>,
    voice: shared::CallVoice,
) -> Fixture {
    let (hangup_sender, hangups) = mpsc::unbounded_channel();
    let (command_sender, model_commands) = mpsc::unbounded_channel();
    let lookup_commands = command_sender.clone();
    let model = Router::new()
        .route("/live", any(move |ws: WebSocketUpgrade| {
            let commands = command_sender.clone();
            async move { ws.on_upgrade(move |mut socket| async move {
                let mut opening_requested = false;
                let mut opening_spoken = false;
                let mut task_session = false;
                let mut input_samples = 0;
                let mut input_audio = Vec::new();
                let mut closing_started = None;
                let mut output = tokio::time::interval(Duration::from_millis(20));
                output.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tokio::select! {
                        message = socket.recv() => {
                            let Some(Ok(Message::Text(text))) = message else { break };
                            let event: Value = serde_json::from_str(&text).unwrap();
                            let mut observed = event.clone();
                            observed["fixture_task_session"] = json!(task_session);
                            let _ = commands.send(observed);
                            match event["type"].as_str().unwrap_or_default() {
                                "session.start" => {
                                    task_session = event["session"]["input"].is_array();
                                    if task_session && scenario == Scenario::TaskConnectionFails {
                                        let _ = socket.send(Message::text(json!({"type":"error","error":{"message":"Fixture task session unavailable"}}).to_string())).await;
                                        break;
                                    }
                                    if task_session && matches!(scenario, Scenario::ReplyWhileConnecting | Scenario::TakeoverWhileConnecting) {
                                        tokio::time::sleep(Duration::from_secs(1)).await;
                                    }
                                    if socket.send(Message::text(json!({"type":"session.started"}).to_string())).await.is_err() { break; }
                                }
                                "session.instructions.append" => {
                                    socket.send(Message::text(json!({"type":"session.instructions.appended", "client_event_id":event["event_id"]}).to_string())).await.unwrap();
                                    if scenario == Scenario::IncomingQuestion && event["event_id"].as_str().is_some_and(|id| id.starts_with("courtside_owner_wait_expired_")) {
                                        socket.send(Message::text(json!({"type":"response.event","event":{
                                            "type":"response.output_item.done","item":{"type":"function_call","name":"ask_user",
                                                "call_id":"tool_fixture_retry","arguments":json!({"question":"Is tomorrow at 3 PM okay?"}).to_string()}
                                        }}).to_string())).await.unwrap();
                                    }
                                    if event["event_id"] == "courtside_opening" || event["event_id"] == "courtside_resume_after_takeover" {
                                        opening_requested = true;
                                        if scenario == Scenario::SemanticClosing {
                                            for (kind, text) in [("input", "Yes, we have vegetarian options."), ("output", "Perfect, that is all I needed. Thank you and goodbye.")] {
                                                socket.send(Message::text(json!({"type":format!("session.{kind}_transcript.delta"),"delta":text,"start_ms":0,"end_ms":1000}).to_string())).await.unwrap();
                                            }
                                            closing_started = Some(std::time::Instant::now());
                                        } else if matches!(scenario, Scenario::SilentClosing | Scenario::ContinuousClosing) {
                                            socket.send(Message::text(json!({
                                                "type":"response.event", "event":{"type":"response.output_item.done", "item":{
                                                    "type":"function_call", "name":"end_call", "call_id":"tool_fixture", "arguments":"{\"reason\":\"The call is complete.\"}"
                                                }}
                                            }).to_string())).await.unwrap();
                                        }
                                    }
                                }
                                "session.input_audio.append" => {
                                    let audio = BASE64.decode(event["audio"].as_str().unwrap()).unwrap();
                                    input_audio.extend_from_slice(&audio);
                                    input_samples += audio.len();
                                    if matches!(scenario, Scenario::IncomingQuestion | Scenario::RecallFound | Scenario::RecallInvalid | Scenario::RecallSlow) && !opening_spoken && audio == [0x22;160] {
                                        opening_spoken = true;
                                        socket.send(Message::text(json!({"type":"session.input_transcript.delta",
                                            "delta":"I'm Sam from the clinic. Would tomorrow at 3 PM work for the owner?",
                                            "start_ms":1000,"end_ms":4000}).to_string())).await.unwrap();
                                        socket.send(Message::text(json!({"type":"response.event","event":{
                                            "type":"response.output_item.done","item":{"type":"function_call","name":"ask_user",
                                                "call_id":"tool_fixture","arguments":json!({"question":if scenario == Scenario::IncomingQuestion {"The clinic is offering tomorrow at 3 PM. Does that work for you?"} else {"What booking reference did I give you for Cedar Dental?"}}).to_string()}
                                        }}).to_string())).await.unwrap();
                                    }
                                    if matches!(scenario, Scenario::IncomingHandoff | Scenario::IncomingMessage)
                                        && !opening_spoken && audio == [0x22;160] {
                                        opening_spoken = true;
                                        socket.send(Message::text(json!({"type":"session.input_transcript.delta",
                                            "delta":if scenario == Scenario::IncomingHandoff {
                                                "I'm Sam from the clinic, calling about an appointment. Could I speak to the owner?"
                                            } else { "I'm Sam from the clinic. Please let the owner know their appointment is confirmed." },
                                            "start_ms":1000,"end_ms":4000}).to_string())).await.unwrap();
                                        socket.send(Message::text(json!({"type":"response.event","event":{
                                            "type":"response.output_item.done", "item":{
                                                "type":"function_call", "name":"update_incoming_call", "call_id":"tool_fixture",
                                                "arguments":json!({"caller_name":"Sam","summary":"Calling from the clinic about an appointment",
                                                    "request_handoff": scenario == Scenario::IncomingHandoff}).to_string()
                                            }
                                        }}).to_string())).await.unwrap();
                                    }
                                    if task_session && !opening_spoken && audio.windows(160).any(|frame| frame == [0x22;160]) {
                                        opening_spoken = true;
                                        let caption = if audio.windows(160).any(|frame| frame == [0x55;160]) { "Hello. I can help you." } else { "Hello." };
                                        socket.send(Message::text(json!({"type":"session.input_transcript.delta","delta":caption,"start_ms":1000,"end_ms":1400}).to_string())).await.unwrap();
                                        socket.send(Message::text(json!({"type":"session.output_audio.delta","delta":BASE64.encode([0x33;160])}).to_string())).await.unwrap();
                                        socket.send(Message::text(json!({"type":"session.output_transcript.delta","delta":"This is Courtside calling on behalf of the user to ask whether vegetarian options are available.","start_ms":2200,"end_ms":4200}).to_string())).await.unwrap();
                                    }
                                    if matches!(scenario, Scenario::CallerFirst | Scenario::CallerInterruptsCue | Scenario::ReplyAfterCue | Scenario::ReplyWhileConnecting | Scenario::TakeoverWhileConnecting | Scenario::TaskConnectionFails)
                                        && !task_session && audio == [0x22;160] {
                                        socket.send(Message::text(json!({"type":"session.input_transcript.delta","delta":"Hello.","start_ms":5000,"end_ms":5200}).to_string())).await.unwrap();
                                    }
                                    if scenario == Scenario::ReplyWhileConnecting && !task_session && audio == [0x55;160] {
                                        socket.send(Message::text(json!({"type":"session.input_transcript.delta","delta":" I can help you.","start_ms":5200,"end_ms":5400}).to_string())).await.unwrap();
                                    }
                                    if !opening_spoken && matches!(scenario, Scenario::InputHello | Scenario::DelayedHelloTranscript | Scenario::ReplyAfterCue | Scenario::MismatchedSyntheticCaption | Scenario::PrematureCompletion)
                                        && input_audio.ends_with(super::super::startup::HELLO_AUDIO) {
                                        opening_spoken = true;
                                        if scenario != Scenario::DelayedHelloTranscript {
                                            let caption = if scenario == Scenario::MismatchedSyntheticCaption { "Hi." } else { "Hello." };
                                            socket.send(Message::text(json!({"type":"session.input_transcript.delta","delta":caption,"start_ms":3200,"end_ms":3400}).to_string())).await.unwrap();
                                        }
                                        if scenario == Scenario::PrematureCompletion {
                                            socket.send(Message::text(json!({
                                                "type":"response.event", "event":{"type":"response.output_item.done", "item":{
                                                    "type":"function_call", "name":"end_call", "call_id":"tool_fixture", "arguments":"{\"reason\":\"I have said hello.\"}"
                                                }}
                                            }).to_string())).await.unwrap();
                                        }
                                        socket.send(Message::text(json!({"type":"session.output_audio.delta","delta":BASE64.encode([0x33;160])}).to_string())).await.unwrap();
                                        socket.send(Message::text(json!({"type":"session.output_transcript.delta","delta":"Hello?","start_ms":3600,"end_ms":4600}).to_string())).await.unwrap();
                                    }
                                    // No recipient audio is ever sent to the bridge. Its
                                    // silence clock must still make the opening possible.
                                    if matches!(scenario, Scenario::Opening | Scenario::RateLimitedOpening) && opening_requested && !opening_spoken && input_samples >= 1600 {
                                        opening_spoken = true;
                                        if scenario == Scenario::RateLimitedOpening {
                                            socket.send(Message::text(json!({"type":"error","error":{"code":"input_audio_rate_limit_exceeded","message":"A single excess input frame was dropped."}}).to_string())).await.unwrap();
                                        }
                                        socket.send(Message::text(json!({"type":"session.output_audio.delta","delta":BASE64.encode([0x22;160])}).to_string())).await.unwrap();
                                    }
                                }
                                "session.commentary.append" => {
                                    if event["event_id"] == "courtside_opening" { opening_requested = true; }
                                    if matches!(scenario,Scenario::RecallFound | Scenario::RecallInvalid | Scenario::RecallSlow | Scenario::IncomingQuestion) {
                                        socket.send(Message::text(json!({"type":"session.output_audio.delta","delta":BASE64.encode([0x33;160])}).to_string())).await.unwrap();
                                        socket.send(Message::text(json!({"type":"session.output_transcript.delta","delta":event["content"],"start_ms":4100,"end_ms":4400}).to_string())).await.unwrap();
                                    }
                                }
                                "response.item.create" => {
                                    if event["item"]["type"] == "function_call_output" {
                                        assert!(event["item"]["call_id"].as_str().unwrap().starts_with("tool_fixture"));
                                        closing_started = Some(std::time::Instant::now());
                                        if scenario == Scenario::IncomingQuestion {
                                            let result: Value = serde_json::from_str(event["item"]["output"].as_str().unwrap()).unwrap();
                                            if result["status"] == "waiting_for_user" && result["already_waiting"] == false {
                                                socket.send(Message::text(json!({"type":"response.event","event":{
                                                    "type":"response.output_item.done","item":{"type":"function_call","name":"ask_user",
                                                        "call_id":"tool_fixture_repeat","arguments":json!({"question":"Could you confirm 3 PM tomorrow?"}).to_string()}
                                                }}).to_string())).await.unwrap();
                                            }
                                        }
                                    }
                                }
                                "session.close" => {
                                    if scenario == Scenario::DelayedHelloTranscript {
                                        let _ = socket.send(Message::text(json!({"type":"session.input_transcript.delta","delta":"Hello.","start_ms":3200,"end_ms":3400}).to_string())).await;
                                    }
                                    let _ = socket.send(Message::text(json!({"type":"session.closed"}).to_string())).await;
                                    break;
                                }
                                _ => {}
                            }
                        }
                        _ = output.tick(), if closing_started.is_some() => {
                            let speaking = scenario == Scenario::ContinuousClosing
                                || (scenario == Scenario::SilentClosing && closing_started.unwrap().elapsed() < Duration::from_millis(400));
                            let sample = if speaking { 0x22 } else { 0xff };
                            if socket.send(Message::text(json!({"type":"session.output_audio.delta","delta":BASE64.encode([sample;160])}).to_string())).await.is_err() { break; }
                        }
                    }
                }
            }) }
        }))
        .route("/completion", post(move |Json(body):Json<Value>| {
            let commands=lookup_commands.clone();
            async move {
                if body["text"]["format"]["name"] == "call_chat_evidence" {
                    let _ = commands.send(json!({"type":"fixture.lookup","body":body}));
                    let input:Value=serde_json::from_str(body["input"][0]["content"].as_str().unwrap()).unwrap();
                    let sources=input["original_owner_messages"].as_array().unwrap();
                    assert!(sources.iter().all(|source| !source["content"].as_str().unwrap().contains("FOREIGN")));
                    let source=sources.iter().find(|source|source["content"].as_str().unwrap().contains("ZX-4827")).unwrap();
                    tokio::time::sleep(if scenario == Scenario::RecallSlow {Duration::from_secs(25)} else {Duration::from_millis(300)}).await;
                    let evidence=json!({"quote":if scenario == Scenario::RecallInvalid {json!("INVENTED-0000")} else {source["content"].clone()},"source_message_id":source["id"]});
                    return Json(json!({"output":[{"type":"message","content":[{"type":"output_text","text":evidence.to_string()}]}]}));
                }
                Json(json!({"output":[{"type":"function_call", "name":"report_call_state",
                    "arguments":json!({"should_end":scenario == Scenario::SemanticClosing,
                    "reason":"The objective is complete and the assistant has said goodbye."}).to_string()}]}))
            }
        }))
        .route("/2010-04-01/Accounts/test/Calls/CAfixture.json", post(move |Form(form): Form<HashMap<String,String>>| {
            let hangups = hangup_sender.clone();
            async move { hangups.send(form).unwrap(); Json(json!({"status":"completed"})) }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_address = listener.local_addr().unwrap();
    let model = tokio::spawn(async move {
        axum::serve(listener, model).await.unwrap();
    });
    let db = crate::db::connect("sqlite::memory:").await.unwrap();
    let user = crate::db::create_user_with_preferences(
        &db,
        "lifecycle@example.com",
        "Fixture",
        "unused",
        voice,
        true,
        None,
    )
    .await
    .unwrap();
    let call = crate::db::create_call(
        &db,
        user.id,
        if matches!(
            scenario,
            Scenario::IncomingHandoff | Scenario::IncomingMessage | Scenario::IncomingQuestion
        ) {
            shared::CallDirection::Inbound
        } else {
            shared::CallDirection::Outbound
        },
        "+14155550100",
        if matches!(
            scenario,
            Scenario::IncomingHandoff | Scenario::IncomingMessage | Scenario::IncomingQuestion
        ) {
            None
        } else if matches!(
            scenario,
            Scenario::RecallFound | Scenario::RecallInvalid | Scenario::RecallSlow
        ) {
            Some("Call Cedar Dental to discuss my booking; use my booking reference if asked.")
        } else {
            Some("Ask whether vegetarian options are available.")
        },
    )
    .await
    .unwrap();
    if matches!(
        scenario,
        Scenario::RecallFound | Scenario::RecallInvalid | Scenario::RecallSlow
    ) {
        let old = crate::db::ensure_current_chat(&db, user.id).await.unwrap();
        crate::db::add_chat_exchange(
            &db,
            user.id,
            old.id,
            "My Cedar Dental booking reference is ZX-4827.",
            "I can use that on a call.",
        )
        .await
        .unwrap();
        let stranger =
            crate::db::create_user(&db, "recall-foreign@example.test", "Other", "unused")
                .await
                .unwrap();
        let foreign = crate::db::ensure_current_chat(&db, stranger.id)
            .await
            .unwrap();
        crate::db::add_chat_exchange(
            &db,
            stranger.id,
            foreign.id,
            "Cedar Dental booking reference FOREIGN-999.",
            "Not yours.",
        )
        .await
        .unwrap();
        let current = crate::db::start_new_chat(&db, user.id).await.unwrap();
        crate::db::set_call_chat_id(&db, call.id, current.id)
            .await
            .unwrap();
    }
    let use_live_model = config_override.is_some();
    let mut config = config_override.unwrap_or_else(Config::for_test);
    if !use_live_model {
        config.test_realtime_url = Some(format!("ws://{model_address}/live"));
    }
    config.test_responses_url = Some(format!("http://{model_address}/completion"));
    if matches!(
        scenario,
        Scenario::RecallFound | Scenario::RecallInvalid | Scenario::RecallSlow
    ) {
        config.chat_model = "gpt-6-luna".into();
    }
    config.test_twilio_rest_url = Some(format!("http://{model_address}"));
    let state = AppState::new(db, config);
    if matches!(
        scenario,
        Scenario::IncomingHandoff | Scenario::IncomingMessage | Scenario::IncomingQuestion
    ) {
        crate::bridge::incoming::announce(&state, call.id)
            .await
            .unwrap();
    }
    state.begin_summary(call.id).await; // no post-call external summary request
    let router = Router::new()
        .route("/media", any(super::media_stream_handler))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Fixture {
        state,
        call_id: call.id,
        address,
        resumed: matches!(
            scenario,
            Scenario::SemanticClosing
                | Scenario::SilentClosing
                | Scenario::ContinuousClosing
                | Scenario::RecallFound
                | Scenario::RecallInvalid
                | Scenario::RecallSlow
        ),
        hangups,
        model_commands,
        app,
        model,
    }
}

type TwilioSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn request_private_question(f: &Fixture) -> (TwilioSocket, shared::CallQuestion) {
    let mut events = f.state.events.subscribe();
    let mut twilio = start(f).await;
    twilio
        .send(ClientMessage::text(
            json!({"event":"media","media":{
                "payload": BASE64.encode([0x22;160])
            }})
            .to_string(),
        ))
        .await
        .unwrap();
    let question = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let shared::ServerEvent::CallQuestionChanged {
                question: Some(question),
                ..
            } = events.recv().await.unwrap()
            {
                return question;
            }
        }
    })
    .await
    .unwrap();
    (twilio, question)
}

async fn owner(f: &Fixture) -> crate::api::auth::AuthedUser {
    let call = crate::db::find_call(&f.state.db, f.call_id)
        .await
        .unwrap()
        .unwrap();
    crate::api::auth::AuthedUser(
        crate::db::find_user_by_id(&f.state.db, call.user_id.unwrap())
            .await
            .unwrap()
            .unwrap(),
    )
}

#[tokio::test]
async fn live_recall_announces_wait_reads_original_chats_and_resumes_with_a_grounded_fact() {
    let mut f = fixture(Scenario::RecallFound).await;
    let mut events = f.state.events.subscribe();
    let (mut phone, question) = request_private_question(&f).await;
    let deadline = f
        .state
        .pending_call_question(f.call_id)
        .await
        .unwrap()
        .deadline;
    let mut hold = false;
    let mut hold_audio = false;
    let mut looked_up = false;
    let mut evidence = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                command = f.model_commands.recv() => {
                    let command = command.unwrap();
                    let id = command["event_id"].as_str().unwrap_or_default();
                    if command["type"] == "fixture.lookup" {
                        looked_up = true;
                        assert_eq!(command["body"]["model"], "gpt-6-luna");
                        let detail=crate::api::routes::call_detail(axum::extract::State(f.state.clone()),
                            owner(&f).await,axum::extract::Path(f.call_id)).await.unwrap().0;
                        assert!(detail.remembering, "reconnects must recover Remembering state");
                        assert_eq!(detail.pending_question, Some(question.clone()));
                        assert_eq!(f.state.pending_call_question(f.call_id).await.unwrap().deadline,deadline);
                    }
                    if id.starts_with("courtside_private_wait_started_") {
                        assert!(command["content"].as_str().unwrap().contains("check that detail"));
                        assert!(command["content"].as_str().unwrap().contains("20 seconds"));
                        hold = true;
                    }
                    if command["type"] == "session.thinking.append" {
                        evidence |= command["content"].as_str().unwrap().contains("ZX-4827");
                    }
                    if id.starts_with("courtside_history_answer_") { break; }
                }
                frame=phone.next()=>{
                    if let Some(Ok(ClientMessage::Text(text)))=frame {
                        let frame:Value=serde_json::from_str(&text).unwrap();
                        if let Some(payload)=frame["media"]["payload"].as_str() {
                            hold_audio |= BASE64.decode(payload).unwrap()==[0x33;160];
                        }
                    }
                }
            }
        }
    }).await.unwrap();
    assert!(hold && hold_audio && looked_up && evidence);
    assert!(f.state.call_question(f.call_id).await.is_none());
    assert!(!f.state.call_remembering(f.call_id).await);
    let mut recall_states = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let shared::ServerEvent::CallRecallChanged {
            call_id,
            remembering,
        } = event
        {
            assert_eq!(call_id, f.call_id);
            recall_states.push(remembering);
        }
    }
    assert_eq!(recall_states, vec![true, false]);
    assert!(f.hangups.try_recv().is_err());
    phone.close(None).await.unwrap();
}

#[tokio::test]
async fn ungrounded_recall_keeps_the_original_owner_deadline_and_never_reaches_voice() {
    let mut f = fixture(Scenario::RecallInvalid).await;
    let (mut phone, question) = request_private_question(&f).await;
    let pending = f.state.pending_call_question(f.call_id).await.unwrap();
    let mut looked_up = false;
    tokio::time::timeout(Duration::from_secs(3),async {
        loop {
            tokio::select! {
                command=f.model_commands.recv()=>{
                    let command=command.unwrap();
                    looked_up |= command["type"]=="fixture.lookup";
                    assert!(!command["content"].as_str().unwrap_or_default().contains("INVENTED-0000"));
                }
                _=phone.next()=>{}
                _=tokio::time::sleep(Duration::from_millis(20))=>{
                    if looked_up && !f.state.call_remembering(f.call_id).await { break; }
                }
            }
        }
    }).await.unwrap();
    let after = f.state.pending_call_question(f.call_id).await.unwrap();
    assert_eq!(after.request.id, question.id);
    assert_eq!(after.deadline, pending.deadline);
    assert!(after.remaining_seconds() <= 20);
    phone.close(None).await.unwrap();
}

#[tokio::test]
async fn owner_answer_cancels_slow_recall_and_phone_audio_continues_during_lookup() {
    let mut f = fixture(Scenario::RecallSlow).await;
    let (mut phone, question) = request_private_question(&f).await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while f.model_commands.recv().await.unwrap()["type"] != "fixture.lookup" {}
    })
    .await
    .unwrap();
    phone
        .send(ClientMessage::text(
            json!({"event":"media","media":{"payload":BASE64.encode([0x23;160])}}).to_string(),
        ))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while f.model_commands.recv().await.unwrap()["audio"] != BASE64.encode([0x23; 160]) {}
    })
    .await
    .unwrap();
    assert!(f.state.call_remembering(f.call_id).await);
    let owner = owner(&f).await;
    let mut request = answer_request(f.call_id, question.id);
    request.message = "Use my corrected reference OWNER-2026.".into();
    let _ = crate::api::chat::send(axum::extract::State(f.state.clone()), owner, Json(request))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut answer_sent = false;
        loop {
            let command = f.model_commands.recv().await.unwrap();
            assert!(!command["event_id"]
                .as_str()
                .unwrap_or_default()
                .starts_with("courtside_history_answer_"));
            answer_sent |= command["content"]
                .as_str()
                .unwrap_or_default()
                .contains("OWNER-2026");
            if command["event_id"]
                .as_str()
                .unwrap_or_default()
                .starts_with("courtside_private_user_answer_")
            {
                assert!(answer_sent);
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(f.state.call_question(f.call_id).await.is_none());
    assert!(!f.state.call_remembering(f.call_id).await);
    phone.close(None).await.unwrap();
}

#[tokio::test]
async fn hung_recall_stops_after_eight_seconds_without_extending_the_twenty_second_wait() {
    let mut f = fixture(Scenario::RecallSlow).await;
    let (mut phone, question) = request_private_question(&f).await;
    let pending = f.state.pending_call_question(f.call_id).await.unwrap();
    let mut looked_up = false;
    tokio::time::timeout(Duration::from_secs(10),async {
        loop {
            tokio::select! {
                command=f.model_commands.recv()=>{looked_up |= command.unwrap()["type"]=="fixture.lookup";}
                _=phone.next()=>{}
                _=tokio::time::sleep(Duration::from_millis(20))=>{
                    if looked_up && !f.state.call_remembering(f.call_id).await {break;}
                }
            }
        }
    }).await.unwrap();
    let after = f.state.pending_call_question(f.call_id).await.unwrap();
    assert_eq!(after.request.id, question.id);
    assert_eq!(after.deadline, pending.deadline);
    assert!((10..=12).contains(&after.remaining_seconds()));
    assert!(f.hangups.try_recv().is_err());
    phone.close(None).await.unwrap();
}

fn answer_request(call_id: Uuid, question_id: Uuid) -> shared::SendChatRequest {
    shared::SendChatRequest {
        message: "Tomorrow at 3 PM works for me.".into(),
        request_id: None,
        call_answer: Some(shared::CallAnswerTarget {
            call_id,
            question_id,
        }),
    }
}

#[tokio::test]
async fn a_private_answer_is_owned_and_returns_to_its_call_even_from_another_chat() {
    let mut f = fixture(Scenario::IncomingQuestion).await;
    let (mut twilio, question) = request_private_question(&f).await;
    let user = owner(&f).await;
    let other_chat = crate::db::ensure_current_chat(&f.state.db, user.0.id)
        .await
        .unwrap();
    assert_ne!(other_chat.id, question.chat_id);
    let detail = crate::api::routes::call_detail(
        axum::extract::State(f.state.clone()),
        crate::api::auth::AuthedUser(user.0.clone()),
        axum::extract::Path(f.call_id),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(detail.pending_question, Some(question.clone()));

    let stranger =
        crate::db::create_user(&f.state.db, "stranger@example.com", "Stranger", "unused")
            .await
            .unwrap();
    let denied = crate::api::chat::send(
        axum::extract::State(f.state.clone()),
        crate::api::auth::AuthedUser(stranger),
        Json(answer_request(f.call_id, question.id)),
    )
    .await
    .unwrap_err();
    assert_eq!(denied.0, axum::http::StatusCode::NOT_FOUND);
    let stale = crate::api::chat::send(
        axum::extract::State(f.state.clone()),
        crate::api::auth::AuthedUser(user.0.clone()),
        Json(answer_request(f.call_id, Uuid::new_v4())),
    )
    .await
    .unwrap_err();
    assert_eq!(stale.0, axum::http::StatusCode::CONFLICT);
    assert_eq!(
        f.state.call_question(f.call_id).await,
        Some(question.clone())
    );

    let response = crate::api::chat::send(
        axum::extract::State(f.state.clone()),
        crate::api::auth::AuthedUser(user.0.clone()),
        Json(answer_request(f.call_id, question.id)),
    )
    .await
    .unwrap()
    .0;
    assert!(response.started_call.is_none());
    assert!(f.state.call_question(f.call_id).await.is_none());
    let duplicate = crate::api::chat::send(
        axum::extract::State(f.state.clone()),
        user,
        Json(answer_request(f.call_id, question.id)),
    )
    .await
    .unwrap_err();
    assert_eq!(duplicate.0, axum::http::StatusCode::CONFLICT);
    assert!(
        crate::db::chat_history_for(&f.state.db, other_chat.user_id, other_chat.id)
            .await
            .unwrap()
            .is_empty()
    );
    let history = crate::db::chat_history_for(&f.state.db, other_chat.user_id, question.chat_id)
        .await
        .unwrap();
    assert!(history
        .iter()
        .any(|message| message.id == response.user_message.id));
    assert_eq!(
        history
            .iter()
            .filter(|message| message.content.contains("other party asked"))
            .count(),
        1
    );

    let mut data_sent = false;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            tokio::select! {
                command = f.model_commands.recv() => {
                    let command = command.unwrap();
                    if command["type"] == "session.thinking.append" {
                        data_sent |= command["content"].as_str().unwrap().contains("Tomorrow at 3 PM works");
                    }
                    if command["event_id"].as_str().is_some_and(|id| id.starts_with("courtside_private_user_answer_")) {
                        assert!(data_sent);
                        break;
                    }
                }
                _ = twilio.next() => {}
            }
        }
    }).await.unwrap();
    assert!(f.hangups.try_recv().is_err());
    assert!(!f.state.expire_call_question(f.call_id, question.id).await);
    twilio.close(None).await.unwrap();
}

#[tokio::test]
async fn private_wait_counts_down_then_expires_without_reasking_or_accepting_a_late_reply() {
    let mut f = fixture(Scenario::IncomingQuestion).await;
    let (mut twilio, question) = request_private_question(&f).await;
    let deadline = f
        .state
        .pending_call_question(f.call_id)
        .await
        .unwrap()
        .deadline;
    let remaining = question
        .expires_at
        .signed_duration_since(chrono::Utc::now())
        .num_milliseconds();
    assert!(
        (1..=20_000).contains(&remaining),
        "chat wait must be at most twenty seconds"
    );
    let mut countdowns = Vec::new();
    let mut announced_wait = false;
    let mut repeated_question = false;
    let mut expired = false;
    let mut timeout_speech = false;
    let mut retry_rejected = false;
    let mut timeout_audio = false;
    // Drain both sockets: otherwise the fixture's phone playback would apply
    // backpressure and obscure whether the application deadline actually ran.
    tokio::time::timeout(Duration::from_secs(26), async {
        loop {
            tokio::select! {
                command = f.model_commands.recv() => {
                    let command = command.unwrap();
                    let id = command["event_id"].as_str().unwrap_or_default();
                    if id.starts_with("courtside_private_wait_started_") {
                        assert!(command["content"].as_str().unwrap().contains("20 seconds"));
                        announced_wait = true;
                    }
                    if command["type"] == "session.commentary.append" && id.starts_with("courtside_owner_wait_") {
                        countdowns.push(id.split('_').nth(3).unwrap().parse::<u64>().unwrap());
                    }
                    if id.starts_with("courtside_owner_timeout_spoken_") { timeout_speech = true; }
                    if id.starts_with("courtside_owner_wait_expired_") {
                        assert!(tokio::time::Instant::now() < deadline + Duration::from_millis(500),
                            "wait expiry must not wait for the next silence-watchdog tick");
                        expired = true;
                    }
                    if command["item"]["type"] == "function_call_output" {
                        let result: Value = serde_json::from_str(command["item"]["output"].as_str().unwrap()).unwrap();
                        if command["item"]["call_id"] == "tool_fixture_repeat" {
                            assert_eq!(result["already_waiting"], true);
                            assert_eq!(f.state.call_question(f.call_id).await, Some(question.clone()));
                            repeated_question = true;
                        }
                        if command["item"]["call_id"] == "tool_fixture_retry" {
                            assert!(expired);
                            assert_eq!(result["status"], "unavailable");
                            retry_rejected = true;
                        }
                    }
                }
                frame = twilio.next() => {
                    let frame=frame.unwrap().unwrap();
                    if let ClientMessage::Text(text)=frame {
                        let frame:Value=serde_json::from_str(&text).unwrap();
                        if let Some(payload)=frame["media"]["payload"].as_str() {
                            timeout_audio |= expired && super::audio_is_audible(payload);
                        }
                    }
                }
            }
            if retry_rejected && timeout_speech && timeout_audio { break; }
        }
    }).await.unwrap();
    assert!(
        timeout_speech && timeout_audio,
        "expired wait must produce actual phone audio"
    );
    assert!(announced_wait && repeated_question && expired);
    assert_eq!(countdowns.len(), 1);
    assert!((5..=10).contains(&countdowns[0]));
    assert!(f.state.call_question(f.call_id).await.is_none());
    assert!(f.state.call_question_timed_out(f.call_id).await);
    assert!(
        f.hangups.try_recv().is_err(),
        "no silent-line hangup while waiting for the owner"
    );
    let user = owner(&f).await;
    let late = crate::api::chat::send(
        axum::extract::State(f.state.clone()),
        user,
        Json(answer_request(f.call_id, question.id)),
    )
    .await
    .unwrap_err();
    assert_eq!(late.0, axum::http::StatusCode::CONFLICT);
    twilio.close(None).await.unwrap();
}

#[tokio::test]
async fn caller_disconnect_cancels_a_private_question() {
    let f = fixture(Scenario::IncomingQuestion).await;
    let (mut twilio, _) = request_private_question(&f).await;
    twilio
        .send(ClientMessage::text(
            json!({"event":"stop","stop":{"callSid":"CAfixture"}}).to_string(),
        ))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while f.state.call_question(f.call_id).await.is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

/// Actual GPT-Live speech and private chat reply on a local phone fixture.
/// No Twilio account, real phone, or real account owner is contacted.
#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "uses the configured OpenAI API account and macOS speech synthesis"]
async fn configured_live_incoming_question_announces_wait_and_uses_the_chat_answer() {
    crate::install_tls_crypto_provider();
    dotenvy::dotenv().ok();
    let mut config = Config::for_test();
    config.openai_api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY");
    config.realtime_model = std::env::var("REALTIME_MODEL").unwrap_or_else(|_| "gpt-live-1".into());
    config.decision_model =
        std::env::var("DECISION_MODEL").unwrap_or_else(|_| "gpt-5.6-terra".into());
    let stem = std::env::temp_dir().join(format!("courtside-private-caller-{}", Uuid::new_v4()));
    let aiff = stem.with_extension("aiff");
    let wav = stem.with_extension("wav");
    assert!(std::process::Command::new("say").arg("-o").arg(&aiff)
        .arg("Hello, this is Sam from the clinic. Would tomorrow at three PM work for an appointment? Can you check with the person you assist?")
        .status().unwrap().success());
    assert!(std::process::Command::new("afconvert")
        .arg(&aiff)
        .arg(&wav)
        .args(["-f", "WAVE", "-d", "ulaw@8000", "-c", "1"])
        .status()
        .unwrap()
        .success());
    let data = std::fs::read(&wav).unwrap();
    let offset = data.windows(4).position(|bytes| bytes == b"data").unwrap();
    let count = u32::from_le_bytes(data[offset + 4..offset + 8].try_into().unwrap()) as usize;
    let audio = data[offset + 8..offset + 8 + count].to_vec();
    let _ = std::fs::remove_file(aiff);
    let _ = std::fs::remove_file(wav);

    let f = fixture_with_config(
        Scenario::IncomingQuestion,
        Some(config),
        shared::CallVoice::Marin,
    )
    .await;
    let mut events = f.state.events.subscribe();
    let mut twilio = start(&f).await;
    let mut chunks = audio.chunks(160);
    let mut clock = tokio::time::interval(Duration::from_millis(20));
    clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut question = None;
    let mut wait_text = String::new();
    let mut reply_text = String::new();
    let mut answered = false;
    let mut phone_audio = 0;
    let result = tokio::time::timeout(Duration::from_secs(50), async {
        loop {
            tokio::select! {
                _ = clock.tick() => {
                    let frame = chunks.next().unwrap_or(&[0xff;160]);
                    twilio.send(ClientMessage::text(json!({"event":"media","media":{"payload":BASE64.encode(frame)}}).to_string())).await.map_err(|error| error.to_string())?;
                }
                event = events.recv() => match event.map_err(|error| error.to_string())? {
                    shared::ServerEvent::CallQuestionChanged { question: Some(request), .. } => {
                        assert!(request.question.to_lowercase().contains("tomorrow"));
                        question = Some(request);
                    }
                    shared::ServerEvent::TranscriptDelta { speaker: shared::Speaker::Assistant, delta, .. } => {
                        if answered { reply_text.push_str(&delta); } else { wait_text.push_str(&delta); }
                    }
                    shared::ServerEvent::CallStatusChanged { status: shared::CallStatus::Completed | shared::CallStatus::Failed, .. } => {
                        return Err("call ended before the private reply was spoken".into());
                    }
                    _ => {}
                },
                frame = twilio.next() => match frame {
                    Some(Ok(ClientMessage::Text(text))) => {
                        let event: Value = serde_json::from_str(&text).unwrap();
                        if event["event"] == "media" {
                            phone_audio += BASE64.decode(event["media"]["payload"].as_str().unwrap()).unwrap().len();
                        }
                    }
                    Some(Err(error)) => return Err(error.to_string()),
                    None => return Err("phone stream closed".into()),
                    _ => {}
                }
            }
            let wait = wait_text.to_lowercase();
            if !answered && (wait.contains("20") || wait.contains("twenty")) {
                if let Some(request) = question.as_ref() {
                    let mut reply = answer_request(f.call_id, request.id);
                    reply.message = "Tomorrow doesn't work. Ask whether Thursday at 11 AM is available.".into();
                    let _ = crate::api::chat::send(axum::extract::State(f.state.clone()), owner(&f).await, Json(reply))
                        .await.map_err(|error| error.1)?;
                    answered = true;
                }
            }
            let spoken = reply_text.to_lowercase();
            if answered && spoken.contains("thursday") && (spoken.contains("11") || spoken.contains("eleven")) {
                return Ok::<_, String>(());
            }
        }
    }).await;
    let _ = twilio.close(None).await;
    assert!(
        matches!(result, Ok(Ok(()))),
        "{result:?}; wait={wait_text}; reply={reply_text}"
    );
    assert!(phone_audio > 0);
    assert!(f.state.call_question(f.call_id).await.is_none());
}

async fn request_incoming_handoff(f: &Fixture) -> (TwilioSocket, shared::CallHandoffRequest) {
    let mut events = f.state.events.subscribe();
    let mut twilio = start(f).await;
    twilio
        .send(ClientMessage::text(
            json!({"event":"media","media":{"payload":BASE64.encode([0x22;160])}}).to_string(),
        ))
        .await
        .unwrap();
    let request = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let shared::ServerEvent::CallHandoffChanged {
                request: Some(request),
                ..
            } = events.recv().await.unwrap()
            {
                break request;
            }
        }
    })
    .await
    .expect("model tool must invite the owner");
    (twilio, request)
}

#[tokio::test]
async fn incoming_caller_updates_do_not_ring_the_owner_without_a_handoff_request() {
    let mut f = fixture(Scenario::IncomingMessage).await;
    let mut events = f.state.events.subscribe();
    let mut twilio = start(&f).await;
    twilio
        .send(ClientMessage::text(
            json!({"event":"media","media":{"payload":BASE64.encode([0x22;160])}}).to_string(),
        ))
        .await
        .unwrap();
    let message = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match events.recv().await.unwrap() {
                shared::ServerEvent::ChatMessageAdded { message, .. } => break message,
                shared::ServerEvent::CallHandoffChanged { .. } => {
                    panic!("a normal caller update must not ring")
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert!(message.content.contains("Sam"));
    assert!(message.content.contains("appointment"));
    assert!(f.state.call_handoff(f.call_id).await.is_none());
    assert!(f.hangups.try_recv().is_err());
    finish_and_read_transcript(&f, &mut twilio).await;
}

#[tokio::test]
async fn incoming_handoff_keeps_ai_until_acceptance_and_joins_the_same_stream() {
    use super::super::voice::{VoiceCommand, VoiceOutput};
    let mut f = fixture(Scenario::IncomingHandoff).await;
    let (mut twilio, request) = request_incoming_handoff(&f).await;
    assert_eq!(request.caller_name, "Sam");
    assert_eq!(
        f.state.call_mode(f.call_id).await,
        shared::CallMode::Assistant
    );
    assert!(f.hangups.try_recv().is_err());
    let sender = f.state.voice_relay(f.call_id).await.unwrap();
    let (output, mut received) = mpsc::channel(16);
    let (reply, joined) = tokio::sync::oneshot::channel();
    let session = Uuid::new_v4();
    sender
        .send(VoiceCommand::Join {
            session,
            output,
            reply,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), joined)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(f.state.call_handoff(f.call_id).await.is_none());
    assert_eq!(
        f.state.call_mode(f.call_id).await,
        shared::CallMode::InAppTakeover
    );
    twilio
        .send(ClientMessage::text(
            json!({"event":"media","media":{"payload":BASE64.encode([0x77;160])}}).to_string(),
        ))
        .await
        .unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(2), received.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(frame, VoiceOutput::Audio(audio) if audio == [0x77;160]));
    sender
        .send(VoiceCommand::Microphone {
            session,
            audio: vec![0x66; 160],
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let frame: Value =
                serde_json::from_str(twilio.next().await.unwrap().unwrap().to_text().unwrap())
                    .unwrap();
            if frame["event"] == "media" && frame["media"]["payload"] == BASE64.encode([0x66; 160])
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    let (reply, returned) = tokio::sync::oneshot::channel();
    sender
        .send(VoiceCommand::Leave {
            session: Some(session),
            reply: Some(reply),
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(4), returned)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        f.state.call_mode(f.call_id).await,
        shared::CallMode::Assistant
    );
    // A returned incoming call must retain its screening/update tool.
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut starts = 0;
        while let Some(command) = f.model_commands.recv().await {
            if command["type"] == "session.start" && command["session"]["delegation"].is_object() {
                starts += 1;
                assert!(command["session"]["delegation"]["responses"]["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|tool| tool["name"] == "update_incoming_call"));
                if starts == 2 {
                    break;
                }
            }
        }
    })
    .await
    .unwrap();
    assert!(
        f.hangups.try_recv().is_err(),
        "handoff never dials or redirects a phone leg"
    );
    finish_and_read_transcript(&f, &mut twilio).await;
}

#[tokio::test]
async fn only_the_owner_can_decline_and_the_caller_stays_with_courtside() {
    let mut f = fixture(Scenario::IncomingHandoff).await;
    let (mut twilio, request) = request_incoming_handoff(&f).await;
    let call = crate::db::find_call(&f.state.db, f.call_id)
        .await
        .unwrap()
        .unwrap();
    let owner = crate::db::find_user_by_id(&f.state.db, call.user_id.unwrap())
        .await
        .unwrap()
        .unwrap();
    let other =
        crate::db::create_user(&f.state.db, "other-incoming@example.com", "Other", "unused")
            .await
            .unwrap();
    for (user, request_id, status) in [
        (other, request.id, axum::http::StatusCode::NOT_FOUND),
        (
            owner.clone(),
            Uuid::new_v4(),
            axum::http::StatusCode::CONFLICT,
        ),
    ] {
        let result = crate::api::routes::decline_call_handoff(
            axum::extract::State(f.state.clone()),
            crate::api::auth::AuthedUser(user),
            axum::extract::Path(f.call_id),
            Json(shared::DeclineCallHandoffRequest { request_id }),
        )
        .await;
        assert_eq!(result.unwrap_err().0, status);
        assert!(f.state.call_handoff(f.call_id).await.is_some());
    }
    let _ = crate::api::routes::decline_call_handoff(
        axum::extract::State(f.state.clone()),
        crate::api::auth::AuthedUser(owner),
        axum::extract::Path(f.call_id),
        Json(shared::DeclineCallHandoffRequest {
            request_id: request.id,
        }),
    )
    .await
    .unwrap();
    assert!(f.state.call_handoff(f.call_id).await.is_none());
    assert_eq!(
        f.state.call_mode(f.call_id).await,
        shared::CallMode::Assistant
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(command) = f.model_commands.recv().await {
            if command["type"] == "session.instructions.append"
                && command["event_id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("courtside_incoming_unavailable_"))
            {
                assert!(command["content"]
                    .as_str()
                    .unwrap()
                    .contains("take a message"));
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(f.hangups.try_recv().is_err());
    finish_and_read_transcript(&f, &mut twilio).await;
}

#[tokio::test]
async fn unanswered_incoming_invitation_expires_before_offering_a_message() {
    let mut f = fixture(Scenario::IncomingHandoff).await;
    let (mut twilio, _) = request_incoming_handoff(&f).await;
    tokio::time::timeout(Duration::from_secs(47), async {
        loop {
            let command = tokio::select! {
                command = f.model_commands.recv() => command.unwrap(),
                frame = twilio.next() => { frame.unwrap().unwrap(); continue; }
            };
            assert_ne!(
                command["event_id"], "courtside_silence_goodbye",
                "don't hang up while the owner is being invited"
            );
            if command["event_id"]
                .as_str()
                .is_some_and(|id| id.starts_with("courtside_incoming_unavailable_"))
            {
                break;
            }
        }
    })
    .await
    .expect("an unanswered invite must be bounded");
    assert!(f.state.call_handoff(f.call_id).await.is_none());
    assert!(f.hangups.try_recv().is_err());
    finish_and_read_transcript(&f, &mut twilio).await;
}

async fn start(f: &Fixture) -> TwilioSocket {
    let (mut socket, _) = connect_async(format!("ws://{}/media", f.address))
        .await
        .unwrap();
    socket.send(ClientMessage::text(json!({"event":"start","start":{
        "streamSid":"MZfixture", "callSid":"CAfixture", "customParameters":{"call_id":f.call_id,"resumed":if f.resumed { "true" } else { "false" }}
    }}).to_string())).await.unwrap();
    socket
}

async fn closing_marker(socket: &mut TwilioSocket) -> usize {
    tokio::time::timeout(Duration::from_secs(8), async {
        let mut audio_frames = 0;
        loop {
            let event = socket.next().await.unwrap().unwrap();
            let event: Value = serde_json::from_str(event.to_text().unwrap()).unwrap();
            match event["event"].as_str().unwrap() {
                "media" => audio_frames += 1,
                "mark" => {
                    assert_eq!(event["mark"]["name"], super::END_CALL_MARK);
                    return audio_frames;
                }
                other => panic!("unexpected Twilio message {other}"),
            }
        }
    })
    .await
    .expect("closing must have a bounded deadline")
}

async fn acknowledge_marker(socket: &mut TwilioSocket) {
    socket
        .send(ClientMessage::text(
            json!({"event":"mark","mark":{"name":super::END_CALL_MARK}}).to_string(),
        ))
        .await
        .unwrap();
}

async fn assert_hangup(f: &mut Fixture) {
    let form = tokio::time::timeout(Duration::from_secs(6), f.hangups.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(form.get("Status").map(String::as_str), Some("completed"));
    assert!(f.hangups.try_recv().is_err());
}

#[tokio::test]
async fn silent_recipient_gets_an_opening_without_sending_a_single_media_frame() {
    let mut f = fixture(Scenario::Opening).await;
    let mut twilio = start(&f).await;
    let audio = tokio::time::timeout(Duration::from_secs(2), twilio.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let audio: Value = serde_json::from_str(audio.to_text().unwrap()).unwrap();
    assert_eq!(audio["event"], "media");
    assert!(super::audio_is_audible(
        audio["media"]["payload"].as_str().unwrap()
    ));
    let mut openings = 0;
    let mut samples = 0;
    while let Ok(event) = f.model_commands.try_recv() {
        if event["event_id"] == "courtside_opening" {
            openings += 1;
        }
        if event["type"] == "session.input_audio.append" {
            samples += BASE64
                .decode(event["audio"].as_str().unwrap())
                .unwrap()
                .len();
        }
    }
    assert_eq!(openings, 1);
    assert!(samples >= 1600);
    assert_eq!(
        f.state.call_mode(f.call_id).await,
        shared::CallMode::Assistant
    );
    twilio
        .send(ClientMessage::text(r#"{"event":"stop"}"#))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_dropped_input_frame_does_not_disconnect_an_otherwise_connected_call() {
    let f = fixture(Scenario::RateLimitedOpening).await;
    let mut twilio = start(&f).await;
    let frame = tokio::time::timeout(Duration::from_secs(2), twilio.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
    assert_eq!(frame["event"], "media");
    assert_eq!(
        BASE64
            .decode(frame["media"]["payload"].as_str().unwrap())
            .unwrap(),
        [0x22; 160]
    );
    assert_eq!(
        f.state.call_mode(f.call_id).await,
        shared::CallMode::Assistant
    );
    finish_and_read_transcript(&f, &mut twilio).await;
}

async fn read_model_reply(twilio: &mut TwilioSocket) {
    let reply = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = twilio.next().await.unwrap().unwrap();
            let event: Value = serde_json::from_str(event.to_text().unwrap()).unwrap();
            assert_eq!(
                event["event"], "media",
                "no rendered-opening playback marker"
            );
            let audio = BASE64
                .decode(event["media"]["payload"].as_str().unwrap())
                .unwrap();
            if super::audio_is_audible(event["media"]["payload"].as_str().unwrap()) {
                return audio;
            }
        }
    })
    .await
    .expect("the model must respond to the internal caller cue");
    assert_eq!(
        reply, [0x33; 160],
        "only the model's output reaches the phone"
    );
}

async fn finish_and_read_transcript(
    f: &Fixture,
    twilio: &mut TwilioSocket,
) -> Vec<shared::TranscriptSegment> {
    twilio
        .send(ClientMessage::text(r#"{"event":"stop"}"#))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while matches!(twilio.next().await, Some(Ok(ClientMessage::Text(_)))) {}
    })
    .await
    .unwrap();
    crate::db::call_transcript(&f.state.db, f.call_id)
        .await
        .unwrap()
}

#[tokio::test]
async fn input_only_hello_wakes_live_without_playing_or_recording_the_cue() {
    let mut f = fixture(Scenario::InputHello).await;
    let mut twilio = start(&f).await;
    read_model_reply(&mut twilio).await;
    let transcript = finish_and_read_transcript(&f, &mut twilio).await;
    assert!(transcript
        .iter()
        .all(|line| line.speaker != shared::Speaker::Callee));
    assert!(transcript.iter().any(|line| line.text == "Hello?"));
    assert!(transcript
        .iter()
        .all(|line| !line.text.contains("vegetarian")));
    let mut input = Vec::new();
    while let Ok(event) = f.model_commands.try_recv() {
        assert_ne!(event["event_id"], "courtside_recipient_ready");
        if event["type"] == "session.start" {
            assert!(!event["session"]["instructions"]
                .as_str()
                .unwrap()
                .contains("vegetarian"));
            assert!(event["session"]["input"].is_null());
        }
        if event["type"] == "session.input_audio.append" {
            input.extend(BASE64.decode(event["audio"].as_str().unwrap()).unwrap());
        }
    }
    assert_eq!(
        input
            .windows(super::super::startup::HELLO_AUDIO.len())
            .filter(|window| *window == super::super::startup::HELLO_AUDIO)
            .count(),
        1
    );
}

#[tokio::test]
async fn the_call_purpose_waits_for_real_audio_and_a_real_reply() {
    let mut f = fixture(Scenario::ReplyAfterCue).await;
    let mut twilio = start(&f).await;
    read_model_reply(&mut twilio).await;
    while let Ok(event) = f.model_commands.try_recv() {
        if event["type"] == "session.start" {
            assert!(
                event["session"]["input"].is_null(),
                "the task cannot begin before a real reply"
            );
            assert!(!event["session"]["instructions"]
                .as_str()
                .unwrap()
                .contains("vegetarian"));
        }
    }
    twilio
        .send(ClientMessage::text(
            json!({"event":"media","media":{"payload":BASE64.encode([0x22;160])}}).to_string(),
        ))
        .await
        .unwrap();
    read_model_reply(&mut twilio).await;
    let transcript = finish_and_read_transcript(&f, &mut twilio).await;
    assert_eq!(
        transcript
            .iter()
            .filter(|line| line.speaker == shared::Speaker::Callee)
            .count(),
        1
    );
    assert!(transcript.iter().any(|line| line.text == "Hello?"));
    assert!(transcript
        .iter()
        .any(|line| line.text.contains("vegetarian options")));
    let mut ready = 0;
    while let Ok(event) = f.model_commands.try_recv() {
        if event["type"] == "session.start" && event["session"]["input"].is_array() {
            ready += 1;
        }
    }
    assert_eq!(ready, 1);
}

async fn wait_for_task_connection(f: &mut Fixture) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let event = f.model_commands.recv().await.unwrap();
            if event["type"] == "session.start" && event["session"]["input"].is_array() {
                return;
            }
        }
    })
    .await
    .expect("the task session should connect only after the real reply");
}

#[tokio::test]
async fn recipient_speech_during_connection_is_retained_without_duplicate_captions() {
    let mut f = fixture(Scenario::ReplyWhileConnecting).await;
    let mut twilio = start(&f).await;
    twilio
        .send(ClientMessage::text(
            json!({"event":"media","media":{"payload":BASE64.encode([0x22;160])}}).to_string(),
        ))
        .await
        .unwrap();
    wait_for_task_connection(&mut f).await;
    twilio
        .send(ClientMessage::text(
            json!({"event":"media","media":{"payload":BASE64.encode([0x55;160])}}).to_string(),
        ))
        .await
        .unwrap();
    read_model_reply(&mut twilio).await;
    let transcript = finish_and_read_transcript(&f, &mut twilio).await;
    let caller: Vec<_> = transcript
        .iter()
        .filter(|line| line.speaker == shared::Speaker::Callee)
        .collect();
    assert_eq!(caller.len(), 1);
    assert_eq!(caller[0].text, "Hello. I can help you.");
    assert!(transcript.last().unwrap().text.contains("vegetarian"));
    let mut reply_replayed = false;
    while let Ok(event) = f.model_commands.try_recv() {
        if event["type"] == "session.input_audio.append" {
            let audio = BASE64.decode(event["audio"].as_str().unwrap()).unwrap();
            if audio == [0x55; 160] {
                reply_replayed = true;
            }
        }
    }
    assert!(
        reply_replayed,
        "speech during startup must reach the task session"
    );
}

#[tokio::test]
async fn native_takeover_remains_available_while_the_task_session_connects() {
    use super::super::voice::{VoiceCommand, VoiceOutput};
    let mut f = fixture(Scenario::TakeoverWhileConnecting).await;
    let mut twilio = start(&f).await;
    twilio
        .send(ClientMessage::text(
            json!({"event":"media","media":{"payload":BASE64.encode([0x22;160])}}).to_string(),
        ))
        .await
        .unwrap();
    wait_for_task_connection(&mut f).await;
    let sender = f.state.voice_relay(f.call_id).await.unwrap();
    let (output, mut received) = mpsc::channel(16);
    let (reply, joined) = tokio::sync::oneshot::channel();
    let session = Uuid::new_v4();
    sender
        .send(VoiceCommand::Join {
            session,
            output,
            reply,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_millis(800), joined)
        .await
        .expect("takeover must not wait for the model connection")
        .unwrap()
        .unwrap();
    assert_eq!(
        f.state.call_mode(f.call_id).await,
        shared::CallMode::InAppTakeover
    );
    twilio
        .send(ClientMessage::text(
            json!({"event":"media","media":{"payload":BASE64.encode([0x77;160])}}).to_string(),
        ))
        .await
        .unwrap();
    let audio = tokio::time::timeout(Duration::from_secs(2), received.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(audio, VoiceOutput::Audio(audio) if audio == [0x77;160]));
    sender
        .send(VoiceCommand::Microphone {
            session,
            audio: vec![0x66; 160],
        })
        .await
        .unwrap();
    let outbound = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let message = twilio.next().await.unwrap().unwrap();
            let event: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
            if event["event"] == "media" {
                return BASE64
                    .decode(event["media"]["payload"].as_str().unwrap())
                    .unwrap();
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        outbound, [0x66; 160],
        "the user's microphone reaches the same phone stream"
    );
    let transcript = finish_and_read_transcript(&f, &mut twilio).await;
    assert!(transcript
        .iter()
        .all(|line| !line.text.contains("vegetarian")));
    assert!(f.hangups.try_recv().is_err());
}

#[tokio::test]
async fn a_failed_task_connection_retries_then_disconnects_instead_of_getting_stuck() {
    let mut f = fixture(Scenario::TaskConnectionFails).await;
    let mut twilio = start(&f).await;
    twilio
        .send(ClientMessage::text(
            json!({"event":"media","media":{"payload":BASE64.encode([0x22;160])}}).to_string(),
        ))
        .await
        .unwrap();
    closing_marker(&mut twilio).await;
    acknowledge_marker(&mut twilio).await;
    assert_hangup(&mut f).await;
    let mut connections = 0;
    let mut goodbye = false;
    while let Ok(event) = f.model_commands.try_recv() {
        if event["type"] == "session.start" && event["session"]["input"].is_array() {
            connections += 1;
        }
        if event["event_id"] == "courtside_audio_unavailable_goodbye" {
            goodbye = true;
        }
    }
    assert_eq!(connections, 3);
    assert!(
        goodbye,
        "a technical failure must not be described as a missing recipient reply"
    );
}

/// Real GPT-Live through the production bridge, with a local phone socket and
/// synthesized recipient audio. No Twilio account or real phone is called.
#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "uses configured voice service and a simulated caller to measure response latency"]
async fn configured_live_casual_reply_has_no_long_wait() {
    use sea_orm::{ActiveModelTrait, EntityTrait, Set};
    use std::process::Command;
    crate::install_tls_crypto_provider();
    dotenvy::dotenv().ok();
    let mut config = Config::for_test();
    config.openai_api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY");
    config.realtime_model = std::env::var("REALTIME_MODEL").unwrap_or_else(|_| "gpt-live-1".into());
    config.decision_model =
        std::env::var("DECISION_MODEL").unwrap_or_else(|_| "gpt-5.6-terra".into());
    let f = fixture_with_config(
        Scenario::CallerFirst,
        Some(config),
        shared::CallVoice::Marin,
    )
    .await;
    let call = crate::db::entities::calls::Entity::find_by_id(f.call_id)
        .one(&f.state.db)
        .await
        .unwrap()
        .unwrap();
    let mut call: crate::db::entities::calls::ActiveModel = call.into();
    call.task_prompt = Set(Some("Make a brief friendly check-in on the owner's behalf. Ask how they are doing, acknowledge their answer politely, and close naturally. No plans or commitments.".into()));
    call.update(&f.state.db).await.unwrap();
    let stem = std::env::temp_dir().join(format!("courtside-reply-latency-{}", Uuid::new_v4()));
    let aiff = stem.with_extension("aiff");
    let wav = stem.with_extension("wav");
    assert!(Command::new("say")
        .arg("-o")
        .arg(&aiff)
        .arg("I'm good, thanks. How about you?")
        .status()
        .unwrap()
        .success());
    assert!(Command::new("afconvert")
        .arg(&aiff)
        .arg(&wav)
        .args(["-f", "WAVE", "-d", "ulaw@8000", "-c", "1"])
        .status()
        .unwrap()
        .success());
    let bytes = std::fs::read(&wav).unwrap();
    let offset = bytes.windows(4).position(|w| w == b"data").unwrap() + 8;
    let reply = &bytes[offset..];
    let mut events = f.state.events.subscribe();
    let mut phone = start(&f).await;
    let mut clock = tokio::time::interval(Duration::from_millis(20));
    clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let started = tokio::time::Instant::now();
    let mut first_audio = None;
    let mut last_audio = None;
    let mut playback_until = None;
    let mut last_speech_played = None;
    let mut hello_cursor = None;
    let mut hello_sent = false;
    let mut reply_cursor = None;
    let mut reply_sent = false;
    let mut caller_end = None;
    let mut response_audio = None;
    let mut intro = String::new();
    let mut response = String::new();
    let result=tokio::time::timeout(Duration::from_secs(45),async {
        loop {
            tokio::select! {
                _=clock.tick()=>{
                    if !hello_sent && intro.to_lowercase().contains("hello") && last_speech_played.is_some_and(|at:tokio::time::Instant|tokio::time::Instant::now().saturating_duration_since(at)>Duration::from_millis(500)) {
                        hello_sent=true;hello_cursor=Some(0);
                    }
                    if hello_sent && hello_cursor.is_none() && !reply_sent
                        && intro.trim().len()>12
                        && last_speech_played.is_some_and(|at:tokio::time::Instant|tokio::time::Instant::now().saturating_duration_since(at)>Duration::from_millis(700)) {
                        reply_sent=true;reply_cursor=Some(0);
                    }
                    let mut frame=[0xff;160];
                    if let Some(cursor)=hello_cursor {
                        let audio=super::super::startup::HELLO_AUDIO;
                        frame.copy_from_slice(&audio[cursor..cursor+160]);
                        hello_cursor=(cursor+160<audio.len()).then_some(cursor+160);
                    } else if let Some(cursor)=reply_cursor {
                        let end=(cursor+160).min(reply.len());
                        frame[..end-cursor].copy_from_slice(&reply[cursor..end]);
                        if super::audio_is_audible(&BASE64.encode(frame)) {caller_end=Some(tokio::time::Instant::now());}
                        reply_cursor=(end<reply.len()).then_some(end);
                    }
                    phone.send(ClientMessage::text(json!({"event":"media","media":{"payload":BASE64.encode(frame)}}).to_string())).await.unwrap();
                }
                frame=phone.next()=>{
                    let Some(Ok(ClientMessage::Text(text)))=frame else {panic!("phone closed before reply; intro={intro}, response={response}")};
                    let frame:Value=serde_json::from_str(&text).unwrap();
                    if frame["event"]=="media" {
                        let payload=frame["media"]["payload"].as_str().unwrap();
                        let samples=BASE64.decode(payload).unwrap();
                        let now=tokio::time::Instant::now();
                        let playback_start=playback_until.unwrap_or(now).max(now);
                        playback_until=Some(playback_start+Duration::from_secs_f64(samples.len() as f64/8_000.0));
                        if super::audio_is_audible(payload) {
                            first_audio.get_or_insert(playback_start);last_audio=Some(now);last_speech_played=playback_until;
                            if reply_sent && reply_cursor.is_none() {response_audio.get_or_insert(playback_start);}
                        }
                    }
                }
                event=events.recv()=>{
                    if let Ok(shared::ServerEvent::TranscriptDelta {speaker:shared::Speaker::Assistant,delta,..})=event {
                        if reply_sent {response.push_str(&delta);}else{intro.push_str(&delta);}
                    }
                }
            }
            if response_audio.is_some() && !response.trim().is_empty()
                && last_audio.is_some_and(|at|at.elapsed()>Duration::from_millis(700)) {return;}
        }
    }).await;
    finish_and_read_transcript(&f, &mut phone).await;
    let _ = std::fs::remove_file(aiff);
    let _ = std::fs::remove_file(wav);
    assert!(
        result.is_ok(),
        "no useful reply; intro={intro}, response={response}"
    );
    let gap = response_audio
        .unwrap()
        .saturating_duration_since(caller_end.unwrap());
    eprintln!(
        "first hello {:.2}s; casual reply playback {:.2}s after caller stopped; reply={response}",
        first_audio.unwrap().duration_since(started).as_secs_f64(),
        gap.as_secs_f64()
    );
    assert!(
        !response.to_lowercase().contains("calling on behalf"),
        "reply restarted its introduction"
    );
    assert!(
        gap <= Duration::from_secs(3),
        "casual reply took {:.2}s",
        gap.as_secs_f64()
    );
}

#[tokio::test]
#[ignore = "uses the configured OpenAI API account"]
async fn configured_live_bridge_waits_for_recipient_before_explaining_the_call() {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    }
    dotenvy::dotenv().ok();
    let mut config = Config::for_test();
    config.openai_api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY");
    config.realtime_model = std::env::var("REALTIME_MODEL").unwrap_or_else(|_| "gpt-live-1".into());
    config.decision_model =
        std::env::var("DECISION_MODEL").unwrap_or_else(|_| "gpt-5.6-terra".into());
    for voice in [shared::CallVoice::Marin, shared::CallVoice::Cedar] {
        let f = fixture_with_config(Scenario::ReplyAfterCue, Some(config.clone()), voice).await;
        let mut events = f.state.events.subscribe();
        let mut twilio = start(&f).await;
        let started = tokio::time::Instant::now();
        let mut clock = tokio::time::interval(Duration::from_millis(20));
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut check_text = String::new();
        let mut task_text = String::new();
        let mut first_audio = None;
        let mut reply_at = None;
        let mut reply_offset = None;
        let mut task_audio = 0;
        let mut first_task_audio = None;
        let result = tokio::time::timeout(Duration::from_secs(35), async {
            loop {
                tokio::select! {
                    _ = clock.tick() => {
                        let check: String = check_text.chars().filter(|c| c.is_alphabetic()).collect();
                        if reply_at.is_none() && check.eq_ignore_ascii_case("hello")
                            && first_audio.is_some_and(|at| started.elapsed() >= at + Duration::from_secs(5)) {
                            reply_at = Some(started.elapsed());
                            reply_offset = Some(0);
                        }
                        let frame = if let Some(offset) = reply_offset {
                            let audio = super::super::startup::HELLO_AUDIO;
                            let frame = &audio[offset..offset+160];
                            reply_offset = (offset+160 < audio.len()).then_some(offset+160);
                            frame
                        } else { &[0xff;160] };
                        twilio.send(ClientMessage::text(json!({"event":"media","media":{"payload":BASE64.encode(frame)}}).to_string())).await.unwrap();
                    }
                    message = twilio.next() => {
                        let Some(Ok(ClientMessage::Text(text))) = message else {
                            return Err("phone stream closed before the task introduction".to_string());
                        };
                        let event: Value = serde_json::from_str(&text).unwrap();
                        if event["event"] == "media" {
                            let payload = event["media"]["payload"].as_str().unwrap();
                            if super::audio_is_audible(payload) {
                                if reply_at.is_some() { first_task_audio.get_or_insert(started.elapsed()); task_audio += BASE64.decode(payload).unwrap().len(); }
                                else { first_audio.get_or_insert(started.elapsed()); }
                            }
                        }
                    }
                    event = events.recv() => {
                        if let Ok(shared::ServerEvent::TranscriptDelta { speaker: shared::Speaker::Assistant, delta, .. }) = event {
                            if reply_at.is_some() { task_text.push_str(&delta); }
                            else { check_text.push_str(&delta); }
                            let check: String = check_text.chars().filter(|c| c.is_alphabetic()).collect();
                            if !"hello".starts_with(&check.to_lowercase()) {
                                return Err(format!("spoke before a real reply: {check_text}"));
                            }
                        }
                    }
                }
                if reply_at.is_some() && task_audio >= 800 && task_text.to_lowercase().contains("vegetarian") {
                    return Ok(());
                }
            }
        }).await;
        let transcript = finish_and_read_transcript(&f, &mut twilio).await;
        assert!(
            matches!(result, Ok(Ok(()))),
            "{voice:?}: {result:?}; presence={check_text}; task={task_text}"
        );
        let caller: Vec<_> = transcript
            .iter()
            .filter(|line| line.speaker == shared::Speaker::Callee)
            .collect();
        assert_eq!(caller.len(), 1, "the actual reply appears once");
        assert_eq!(
            caller[0]
                .text
                .trim_matches(|c: char| !c.is_alphabetic())
                .to_lowercase(),
            "hello"
        );
        assert_eq!(
            f.state.call_mode(f.call_id).await,
            shared::CallMode::Assistant
        );
        assert!(
            first_audio.unwrap() < Duration::from_secs(5),
            "first hello took too long: {:?}",
            first_audio
        );
        eprintln!(
            "{voice:?}: first hello {:.2}s; first task speech {:.2}s after recipient reply",
            first_audio.unwrap().as_secs_f64(),
            (first_task_audio.unwrap() - reply_at.unwrap()).as_secs_f64()
        );
        eprintln!("{voice:?}: only Hello for five seconds, then call purpose {:.2}s after the simulated recipient reply; actual reply recorded once", (started.elapsed()-reply_at.unwrap()).as_secs_f64());
    }
}

#[tokio::test]
async fn a_mistranscribed_synthetic_hello_cannot_start_the_call_task() {
    let mut f = fixture(Scenario::MismatchedSyntheticCaption).await;
    let mut twilio = start(&f).await;
    read_model_reply(&mut twilio).await;
    let transcript = finish_and_read_transcript(&f, &mut twilio).await;
    assert!(transcript
        .iter()
        .all(|line| line.speaker != shared::Speaker::Callee));
    assert!(transcript
        .iter()
        .all(|line| !line.text.contains("vegetarian")));
    while let Ok(event) = f.model_commands.try_recv() {
        assert_ne!(event["event_id"], "courtside_recipient_ready");
    }
}

#[tokio::test]
async fn saying_hello_does_not_complete_a_call_that_is_waiting_for_a_reply() {
    let mut f = fixture(Scenario::PrematureCompletion).await;
    let mut twilio = start(&f).await;
    read_model_reply(&mut twilio).await;
    assert!(f.hangups.try_recv().is_err());
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let event = f.model_commands.recv().await.unwrap();
            if event["type"] == "response.item.create" {
                let output: Value =
                    serde_json::from_str(event["item"]["output"].as_str().unwrap()).unwrap();
                assert_eq!(output["status"], "waiting_for_recipient");
                break;
            }
        }
    })
    .await
    .expect("premature completion must be deferred");
    finish_and_read_transcript(&f, &mut twilio).await;
    assert!(f.hangups.try_recv().is_err());
}

#[tokio::test]
async fn synthetic_caption_arriving_during_close_stays_out_of_the_transcript() {
    let f = fixture(Scenario::DelayedHelloTranscript).await;
    let mut twilio = start(&f).await;
    read_model_reply(&mut twilio).await;
    let transcript = finish_and_read_transcript(&f, &mut twilio).await;
    assert!(transcript
        .iter()
        .all(|line| line.speaker != shared::Speaker::Callee));
    assert!(transcript
        .iter()
        .any(|line| line.speaker == shared::Speaker::Assistant));
}

#[tokio::test]
async fn real_caller_speech_prevents_the_cue_and_preserves_their_hello() {
    let mut f = fixture(Scenario::CallerFirst).await;
    let mut twilio = start(&f).await;
    twilio
        .send(ClientMessage::text(
            json!({"event":"media","media":{"payload":BASE64.encode([0x22;160])}}).to_string(),
        ))
        .await
        .unwrap();
    read_model_reply(&mut twilio).await;
    // Keep the call open past the fallback deadline.
    tokio::time::sleep(Duration::from_millis(2300)).await;
    let transcript = finish_and_read_transcript(&f, &mut twilio).await;
    assert!(transcript
        .iter()
        .any(|line| line.speaker == shared::Speaker::Callee && line.text == "Hello."));
    while let Ok(event) = f.model_commands.try_recv() {
        if event["type"] == "session.input_audio.append" {
            let audio = BASE64.decode(event["audio"].as_str().unwrap()).unwrap();
            assert!(
                audio.iter().all(|byte| matches!(byte, 0x22 | 0xff)),
                "no synthetic speech after real caller input"
            );
        }
    }
}

#[tokio::test]
async fn real_caller_audio_interrupts_a_cue_already_being_sent() {
    let mut f = fixture(Scenario::CallerInterruptsCue).await;
    let mut twilio = start(&f).await;
    let mut input = Vec::new();
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let event = f.model_commands.recv().await.unwrap();
            if event["type"] == "session.input_audio.append" {
                let audio = BASE64.decode(event["audio"].as_str().unwrap()).unwrap();
                let audible = super::audio_is_audible(event["audio"].as_str().unwrap());
                input.extend(audio);
                if audible {
                    break;
                }
            }
        }
    })
    .await
    .expect("wait until the internal cue starts speaking");
    twilio
        .send(ClientMessage::text(
            json!({"event":"media","media":{"payload":BASE64.encode([0x22;160])}}).to_string(),
        ))
        .await
        .unwrap();
    read_model_reply(&mut twilio).await;
    let transcript = finish_and_read_transcript(&f, &mut twilio).await;
    assert!(transcript
        .iter()
        .any(|line| line.speaker == shared::Speaker::Callee && line.text == "Hello."));
    while let Ok(event) = f.model_commands.try_recv() {
        if event["type"] == "session.input_audio.append" {
            input.extend(BASE64.decode(event["audio"].as_str().unwrap()).unwrap());
        }
    }
    assert!(!input
        .windows(super::super::startup::HELLO_AUDIO.len())
        .any(|window| window == super::super::startup::HELLO_AUDIO));
}

#[tokio::test]
async fn silent_output_after_goodbye_does_not_keep_resetting_hangup() {
    let mut f = fixture(Scenario::SilentClosing).await;
    let mut twilio = start(&f).await;
    assert!(closing_marker(&mut twilio).await > 0);
    acknowledge_marker(&mut twilio).await;
    assert_hangup(&mut f).await;
    // Output generated after the marker must never be queued on the call.
    let next = tokio::time::timeout(Duration::from_secs(3), twilio.next())
        .await
        .unwrap();
    assert!(!matches!(next, Some(Ok(ClientMessage::Text(_)))));
}

#[tokio::test]
async fn endless_final_speech_and_missing_playback_ack_still_disconnect() {
    let mut f = fixture(Scenario::ContinuousClosing).await;
    let mut twilio = start(&f).await;
    assert!(closing_marker(&mut twilio).await > 0);
    // The mock carrier never acknowledges the marker. The fallback must
    // still send a completed request after the finite playback wait.
    assert_hangup(&mut f).await;
}

#[tokio::test]
async fn semantic_completion_disconnects_when_the_live_model_misses_end_call() {
    let mut f = fixture(Scenario::SemanticClosing).await;
    let mut twilio = start(&f).await;
    closing_marker(&mut twilio).await;
    acknowledge_marker(&mut twilio).await;
    assert_hangup(&mut f).await;
    while let Ok(event) = f.model_commands.try_recv() {
        assert_ne!(
            event["type"], "response.item.create",
            "no end_call tool was emitted in this scenario"
        );
    }
}
