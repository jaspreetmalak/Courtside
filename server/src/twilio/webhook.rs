//! Twilio voice webhook: answers a call (inbound or our own outbound leg)
//! with TwiML that connects the call audio to our media-stream WebSocket.

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Form,
};
use serde::Deserialize;
use shared::{CallDirection, CallMode, CallStatus};
use uuid::Uuid;

use crate::state::AppState;

/// Fields Twilio posts to the voice webhook (subset we use).
#[allow(dead_code)] // to/direction kept for logging & future routing
#[derive(Debug, Deserialize)]
pub struct VoiceWebhookForm {
    #[serde(rename = "CallSid")]
    pub call_sid: String,
    #[serde(rename = "From")]
    pub from: Option<String>,
    #[serde(rename = "To")]
    pub to: Option<String>,
    /// Present when we placed the call ourselves (we pass ?call_id=... in the URL).
    #[serde(rename = "Direction")]
    pub direction: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct VoiceQuery {
    /// Our internal call id, present on outbound legs we originated.
    pub call_id: Option<Uuid>,
}

pub async fn voice_webhook(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<VoiceQuery>,
    Form(form): Form<VoiceWebhookForm>,
) -> Response {
    let call_id = match query.call_id {
        // Outbound leg we originated: the row already exists.
        Some(id) => id,
        // True inbound call: create a row now.
        None => {
            let _start_guard = state.call_start.lock().await;
            if let Ok(Some(existing)) = crate::db::find_call_by_sid(&state.db, &form.call_sid).await
            {
                if matches!(existing.status.as_str(), "completed" | "failed") {
                    return twiml_response("<Response><Hangup/></Response>".into());
                }
                return twiml_response(assistant_twiml(&state, existing.id, false));
            }
            let peer = form.from.as_deref().unwrap_or("unknown");
            let Some(to) = form.to.as_deref() else {
                tracing::warn!(call_sid = %form.call_sid, "inbound call missing destination number");
                return twiml_response("<Response><Reject/></Response>".into());
            };
            let owner = match crate::db::owned_number_by_phone(&state.db, to).await {
                Ok(Some(owner)) => owner,
                Ok(None) => {
                    tracing::warn!(%to, call_sid = %form.call_sid, "inbound number is not assigned");
                    return twiml_response("<Response><Reject/></Response>".into());
                }
                Err(err) => {
                    tracing::error!(?err, %to, "failed to resolve inbound number owner");
                    return twiml_response("<Response><Reject/></Response>".into());
                }
            };
            let Some(user_id) = owner.dedicated_user_id else {
                return twiml_response("<Response><Reject/></Response>".into());
            };
            let account = match state.account_status(user_id).await {
                Ok(account) => account,
                Err(err) => {
                    tracing::error!(?err, "failed to load inbound entitlement");
                    return twiml_response("<Response><Reject/></Response>".into());
                }
            };
            let remaining = account.included_seconds_limit - account.included_seconds_used;
            if account.tier != shared::AccountTier::Paid || remaining <= 0 {
                tracing::info!(%user_id, "inbound call rejected: no paid minutes");
                return twiml_response("<Response><Reject/></Response>".into());
            }
            if crate::db::has_active_call(&state.db, user_id)
                .await
                .unwrap_or(true)
            {
                return twiml_response("<Response><Reject reason=\"busy\"/></Response>".into());
            }
            match crate::db::create_metered_call(
                &state.db,
                user_id,
                CallDirection::Inbound,
                peer,
                None,
                Some(owner.id),
                Some("paid"),
                Some(remaining),
                true,
                true,
                shared::CallAiBackend::OpenAiLive,
            )
            .await
            {
                Ok(call) => {
                    if let Err(err) =
                        crate::db::set_call_sid(&state.db, call.id, &form.call_sid).await
                    {
                        tracing::error!(?err, call_id = %call.id, "failed to store incoming call sid");
                        state
                            .set_status_and_broadcast(call.id, CallStatus::Failed)
                            .await;
                        return twiml_response("<Response><Reject/></Response>".into());
                    }
                    // Publish after the call is attached to a private chat so
                    // updates and later viewing survive an event reconnect.
                    if let Err(err) = crate::bridge::incoming::announce(&state, call.id).await {
                        tracing::error!(?err, call_id = %call.id, "failed to announce incoming call");
                        state
                            .set_status_and_broadcast(call.id, CallStatus::Failed)
                            .await;
                        return twiml_response("<Response><Reject/></Response>".into());
                    }
                    call.id
                }
                Err(err) => {
                    tracing::error!(?err, "failed to create inbound call row");
                    return twiml_response("<Response><Reject/></Response>".into());
                }
            }
        }
    };

    if let Err(err) = crate::db::set_call_sid(&state.db, call_id, &form.call_sid).await {
        tracing::warn!(?err, %call_id, "failed to store twilio call sid");
    }
    // An inbound takeover outlives the assistant's WebSocket. Keep watching
    // its parent call so a hangup still finalizes status and total usage.
    if query.call_id.is_none() {
        crate::twilio::rest::monitor_outbound_call(state.clone(), call_id, form.call_sid.clone());
    }
    state
        .set_status_and_broadcast(call_id, CallStatus::Ringing)
        .await;

    state
        .set_call_mode(call_id, shared::CallMode::Assistant)
        .await;
    twiml_response(assistant_twiml(&state, call_id, false))
}

pub async fn resume_webhook(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<VoiceQuery>,
    Form(form): Form<VoiceWebhookForm>,
) -> Response {
    let Some(call_id) = query.call_id else {
        return twiml_response("<Response><Hangup/></Response>".into());
    };
    let matches = crate::db::find_call(&state.db, call_id)
        .await
        .ok()
        .flatten()
        .is_some_and(|call| call.twilio_call_sid.as_deref() == Some(&form.call_sid));
    if !matches {
        return twiml_response("<Response><Hangup/></Response>".into());
    }
    state
        .set_call_mode(call_id, shared::CallMode::Assistant)
        .await;
    twiml_response(assistant_twiml(&state, call_id, true))
}

pub fn assistant_twiml(state: &AppState, call_id: Uuid, resumed: bool) -> String {
    let stream_url = format!("wss://{}/twilio/media", state.config.public_host);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Response>
  <Connect>
    <Stream url="{stream_url}">
      <Parameter name="call_id" value="{call_id}"/>
      <Parameter name="resumed" value="{resumed}"/>
    </Stream>
  </Connect>
</Response>"#
    )
}

/// Twilio Media Streams only reports DTMF from the call to our server; it does
/// not accept outbound DTMF WebSocket messages. Redirecting the active call to
/// this short TwiML sequence presses the selection, then reconnects the same
/// bidirectional assistant stream with its persisted transcript context.
pub fn dtmf_twiml(state: &AppState, call_id: Uuid, digits: &str) -> String {
    let stream_url = format!("wss://{}/twilio/media", state.config.public_host);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Response>
  <Play digits="{digits}"></Play>
  <Connect>
    <Stream url="{stream_url}">
      <Parameter name="call_id" value="{call_id}"/>
      <Parameter name="resumed" value="true"/>
      <Parameter name="resume_reason" value="dtmf"/>
      <Parameter name="pressed_digits" value="{digits}"/>
    </Stream>
  </Connect>
</Response>"#
    )
}

pub fn takeover_twiml(
    state: &AppState,
    call_id: Uuid,
    attempt: Uuid,
    from_number: &str,
    takeover_phone: &str,
) -> String {
    render_takeover_twiml(
        &state.config.public_host,
        call_id,
        attempt,
        from_number,
        takeover_phone,
    )
}

fn render_takeover_twiml(
    host: &str,
    call_id: Uuid,
    attempt: Uuid,
    from_number: &str,
    takeover_phone: &str,
) -> String {
    let observe_url = format!("wss://{host}/twilio/observe");
    let callback_query = format!("call_id={call_id}&amp;attempt={attempt}");
    let status_url = format!("https://{host}/twilio/takeover/status?{callback_query}");
    let finished_url = format!("https://{host}/twilio/takeover/finished?{callback_query}");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Response>
  <Start>
    <Stream name="takeover-{attempt}" url="{observe_url}" track="both_tracks">
      <Parameter name="call_id" value="{call_id}"/>
    </Stream>
  </Start>
  <Dial callerId="{from_number}" answerOnBridge="true" timeout="30" action="{finished_url}" method="POST">
    <Number statusCallback="{status_url}" statusCallbackMethod="POST" statusCallbackEvent="initiated ringing answered completed">{takeover_phone}</Number>
  </Dial>
</Response>"#
    )
}

pub fn assistant_resume_twiml(state: &AppState, call_id: Uuid, attempt: Option<Uuid>) -> String {
    let twiml = assistant_twiml(state, call_id, true);
    match attempt {
        Some(attempt) => twiml.replace(
            "  <Connect>",
            &format!("  <Stop><Stream name=\"takeover-{attempt}\"/></Stop>\n  <Connect>"),
        ),
        None => twiml,
    }
}

#[derive(Debug, Deserialize)]
pub struct TakeoverQuery {
    pub call_id: Uuid,
    pub attempt: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct TakeoverStatusForm {
    #[serde(rename = "ParentCallSid")]
    pub parent_call_sid: String,
    #[serde(rename = "CallStatus")]
    pub call_status: String,
}

pub async fn takeover_status_webhook(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<TakeoverQuery>,
    Form(form): Form<TakeoverStatusForm>,
) -> Response {
    if !active_parent_matches(&state, query.call_id, &form.parent_call_sid).await {
        return StatusCode::NO_CONTENT.into_response();
    }
    let progress = match form.call_status.as_str() {
        "initiated" => Some((
            CallMode::TakeoverPending,
            "Calling your takeover number. Answer the incoming call to join.",
        )),
        "ringing" => Some((
            CallMode::TakeoverPending,
            "Your takeover phone is ringing. Answer it to join the call.",
        )),
        "in-progress" => Some((
            CallMode::Takeover,
            "You're connected. Use Return to AI when you're ready.",
        )),
        // The Dial action reconnects the parent call. Do not mark the parent
        // completed or switch to AI just because its child leg finished.
        _ => None,
    };
    if let Some((mode, message)) = progress {
        state
            .update_takeover(query.call_id, query.attempt, mode, message)
            .await;
    }
    tracing::info!(call_id = %query.call_id, attempt = %query.attempt, status = %form.call_status, "takeover phone status");
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Debug, Deserialize)]
pub struct TakeoverFinishedForm {
    #[serde(rename = "CallSid")]
    pub call_sid: String,
    #[serde(rename = "DialCallStatus")]
    pub dial_call_status: String,
}

pub async fn takeover_finished_webhook(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<TakeoverQuery>,
    Form(form): Form<TakeoverFinishedForm>,
) -> Response {
    if !active_parent_matches(&state, query.call_id, &form.call_sid).await {
        return twiml_response("<Response><Hangup/></Response>".into());
    }
    let message = takeover_result_message(&form.dial_call_status);
    state
        .update_takeover(query.call_id, query.attempt, CallMode::Assistant, message)
        .await;
    tracing::info!(call_id = %query.call_id, attempt = %query.attempt, outcome = %form.dial_call_status, "takeover ended; resuming assistant");
    twiml_response(assistant_resume_twiml(
        &state,
        query.call_id,
        Some(query.attempt),
    ))
}

async fn active_parent_matches(state: &AppState, call_id: Uuid, parent_sid: &str) -> bool {
    crate::db::find_call(&state.db, call_id)
        .await
        .ok()
        .flatten()
        .is_some_and(|call| {
            call.twilio_call_sid.as_deref() == Some(parent_sid)
                && matches!(call.status.as_str(), "ringing" | "in_progress")
        })
}

fn takeover_result_message(status: &str) -> &'static str {
    match status {
        "busy" => "Your takeover phone was busy. Courtside has the call again.",
        "no-answer" => "Your takeover phone wasn't answered. Courtside has the call again.",
        "failed" => "Twilio couldn't reach your takeover phone. Check the number in Account. Courtside has the call again.",
        "canceled" => "Takeover canceled. Courtside has the call again.",
        _ => "Courtside has the call again.",
    }
}

#[derive(Debug, Deserialize)]
pub struct StatusWebhookForm {
    #[serde(rename = "CallSid")]
    pub call_sid: String,
    #[serde(rename = "CallStatus")]
    pub call_status: String,
    #[serde(rename = "CallDuration", default)]
    pub call_duration: Option<i64>,
}

/// Twilio's completed callback is the authoritative outbound usage record.
/// Matching both our opaque call id and Twilio's SID makes retries idempotent
/// and prevents a caller from charging time to another row.
pub async fn status_webhook(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<VoiceQuery>,
    Form(form): Form<StatusWebhookForm>,
) -> Response {
    let Some(call_id) = query.call_id else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let call = match crate::db::find_call(&state.db, call_id).await {
        Ok(Some(call)) if call.twilio_call_sid.as_deref() == Some(&form.call_sid) => call,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let seconds = form.call_duration.unwrap_or(0);
    if let Err(err) = crate::db::finalize_call_usage(&state.db, call_id, seconds).await {
        tracing::error!(?err, %call_id, "failed to finalize call usage");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let status = match form.call_status.as_str() {
        "completed" => CallStatus::Completed,
        _ => CallStatus::Failed,
    };
    if call.status != "completed" {
        state.set_status_and_broadcast(call_id, status).await;
    }
    let summary_state = state.clone();
    tokio::spawn(async move {
        // Give the final media stream a moment to flush transcript deltas.
        tokio::time::sleep(std::time::Duration::from_millis(2_500)).await;
        if let Err(err) = crate::bridge::call_bridge::summarize_call(&summary_state, call_id).await
        {
            tracing::error!(?err, %call_id, "failed to summarize completed Twilio call");
        }
    });
    StatusCode::NO_CONTENT.into_response()
}

fn twiml_response(twiml: String) -> Response {
    ([(header::CONTENT_TYPE, "text/xml")], twiml).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn owned_incoming_number_creates_one_private_call_on_webhook_retries() {
        let db = crate::db::connect("sqlite::memory:").await.unwrap();
        let user = crate::db::create_user(&db, "screening@example.com", "Owner", "unused")
            .await
            .unwrap();
        let number = "+14155550101";
        crate::db::assign_owned_number(&db, user.id, "PNincoming", number, "Incoming")
            .await
            .unwrap();
        crate::db::upsert_subscription(
            &db,
            user.id,
            "test",
            "incoming-transaction",
            "active",
            chrono::Utc::now() - chrono::Duration::days(1),
            chrono::Utc::now() + chrono::Duration::days(1),
            true,
            "sandbox",
        )
        .await
        .unwrap();
        let mut config = crate::config::Config::for_test();
        config.test_twilio_rest_url = Some("http://127.0.0.1:9".into());
        let state = AppState::new(db, config);
        let account = state.account_status(user.id).await.unwrap();
        let mut events = state.events.subscribe();
        for _ in 0..2 {
            let response = voice_webhook(
                State(state.clone()),
                axum::extract::Query(VoiceQuery { call_id: None }),
                Form(VoiceWebhookForm {
                    call_sid: "CAincoming".into(),
                    from: Some("+14155550100".into()),
                    to: Some(number.into()),
                    direction: Some("inbound".into()),
                }),
            )
            .await;
            let xml = axum::body::to_bytes(response.into_body(), 8192)
                .await
                .unwrap();
            assert!(String::from_utf8(xml.to_vec())
                .unwrap()
                .contains("<Connect>"));
        }
        let calls = crate::db::list_calls(&state.db, user.id).await.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].direction, CallDirection::Inbound);
        let call = crate::db::find_call(&state.db, calls[0].id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(call.twilio_call_sid.as_deref(), Some("CAincoming"));
        assert_eq!(call.quota_cap_seconds, Some(account.included_seconds_limit));
        assert!(call.chat_id.is_some());
        let incoming = std::iter::from_fn(|| events.try_recv().ok())
            .filter(|event| matches!(event, shared::ServerEvent::IncomingCall { .. }))
            .count();
        assert_eq!(incoming, 1, "a webhook retry must not ring the app twice");
    }

    #[tokio::test]
    async fn incoming_routing_rejects_unassigned_or_unentitled_numbers() {
        let db = crate::db::connect("sqlite::memory:").await.unwrap();
        let user = crate::db::create_user(&db, "free-incoming@example.com", "Owner", "unused")
            .await
            .unwrap();
        crate::db::assign_owned_number(&db, user.id, "PNfree", "+14155550101", "Incoming")
            .await
            .unwrap();
        let state = AppState::new(db, crate::config::Config::for_test());
        for to in ["+14155550101", "+14155550102"] {
            let response = voice_webhook(
                State(state.clone()),
                axum::extract::Query(VoiceQuery { call_id: None }),
                Form(VoiceWebhookForm {
                    call_sid: "CArejected".into(),
                    from: Some("+14155550100".into()),
                    to: Some(to.into()),
                    direction: Some("inbound".into()),
                }),
            )
            .await;
            let xml = axum::body::to_bytes(response.into_body(), 8192)
                .await
                .unwrap();
            assert!(String::from_utf8(xml.to_vec()).unwrap().contains("<Reject"));
        }
        assert!(crate::db::list_calls(&state.db, user.id)
            .await
            .unwrap()
            .is_empty());
    }

    async fn live_call() -> (AppState, Uuid, crate::db::entities::users::Model) {
        let db = crate::db::connect("sqlite::memory:").await.unwrap();
        let user = crate::db::create_user(&db, "takeover@example.com", "Owner", "unused")
            .await
            .unwrap();
        let call = crate::db::create_call(
            &db,
            user.id,
            CallDirection::Outbound,
            "+14155550100",
            Some("Test handoff"),
        )
        .await
        .unwrap();
        crate::db::set_call_sid(&db, call.id, "CAparent")
            .await
            .unwrap();
        crate::db::set_call_status(&db, call.id, CallStatus::InProgress)
            .await
            .unwrap();
        (
            AppState::new(db, crate::config::Config::for_test()),
            call.id,
            user,
        )
    }

    async fn status(state: &AppState, id: Uuid, attempt: Uuid, parent: &str, status: &str) {
        takeover_status_webhook(
            State(state.clone()),
            axum::extract::Query(TakeoverQuery {
                call_id: id,
                attempt,
            }),
            Form(TakeoverStatusForm {
                parent_call_sid: parent.into(),
                call_status: status.into(),
            }),
        )
        .await;
    }

    #[tokio::test]
    async fn takeover_waits_for_answer_and_late_ringing_cannot_undo_connection() {
        let (state, id, _) = live_call().await;
        let attempt = state.begin_takeover(id).await;
        assert_eq!(state.call_mode(id).await, CallMode::TakeoverPending);
        status(&state, id, attempt, "CAparent", "ringing").await;
        assert!(state.call_control(id).await.message.contains("ringing"));
        status(&state, id, attempt, "CAparent", "in-progress").await;
        status(&state, id, attempt, "CAparent", "ringing").await;
        assert_eq!(state.call_mode(id).await, CallMode::Takeover);
        assert!(state
            .call_control(id)
            .await
            .message
            .contains("You're connected"));
    }

    #[tokio::test]
    async fn canceled_attempts_and_unrelated_parents_cannot_change_control() {
        let (state, id, _) = live_call().await;
        let old = state.begin_takeover(id).await;
        state
            .set_call_control(id, CallMode::Assistant, "Canceled")
            .await;
        status(&state, id, old, "CAparent", "in-progress").await;
        assert_eq!(state.call_mode(id).await, CallMode::Assistant);
        let current = state.begin_takeover(id).await;
        status(&state, id, old, "CAparent", "in-progress").await;
        status(&state, id, current, "CAunrelated", "in-progress").await;
        assert_eq!(state.call_mode(id).await, CallMode::TakeoverPending);
    }

    #[tokio::test]
    async fn no_answer_returns_to_ai_without_ending_the_parent_call() {
        let (state, id, _) = live_call().await;
        let attempt = state.begin_takeover(id).await;
        let response = takeover_finished_webhook(
            State(state.clone()),
            axum::extract::Query(TakeoverQuery {
                call_id: id,
                attempt,
            }),
            Form(TakeoverFinishedForm {
                call_sid: "CAparent".into(),
                dial_call_status: "no-answer".into(),
            }),
        )
        .await;
        let xml = String::from_utf8(
            axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(xml.contains(&format!(
            "<Stop><Stream name=\"takeover-{attempt}\"/></Stop>"
        )));
        assert!(xml.contains("wss://example.com/twilio/media"));
        assert!(xml.contains("name=\"resumed\" value=\"true\""));
        assert_eq!(state.call_mode(id).await, CallMode::Assistant);
        assert!(state
            .call_control(id)
            .await
            .message
            .contains("wasn't answered"));
        assert_eq!(
            crate::db::find_call(&state.db, id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "in_progress"
        );
    }

    #[tokio::test]
    async fn ended_calls_cannot_be_resurrected_by_takeover_callbacks() {
        let (state, id, _) = live_call().await;
        let attempt = state.begin_takeover(id).await;
        crate::db::set_call_status(&state.db, id, CallStatus::Completed)
            .await
            .unwrap();
        status(&state, id, attempt, "CAparent", "in-progress").await;
        assert_ne!(state.call_mode(id).await, CallMode::Takeover);
        let response = takeover_finished_webhook(
            State(state.clone()),
            axum::extract::Query(TakeoverQuery {
                call_id: id,
                attempt,
            }),
            Form(TakeoverFinishedForm {
                call_sid: "CAparent".into(),
                dial_call_status: "completed".into(),
            }),
        )
        .await;
        let xml = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(xml, "<Response><Hangup/></Response>");
    }

    #[tokio::test]
    async fn tapping_before_connection_gives_an_actionable_response() {
        let (state, id, user) = live_call().await;
        crate::db::set_call_status(&state.db, id, CallStatus::Ringing)
            .await
            .unwrap();
        let err = crate::api::routes::takeover_call(
            State(state),
            crate::api::auth::AuthedUser(user),
            axum::extract::Path(id),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert!(err.1.contains("hasn't connected yet"));
    }

    #[test]
    fn takeover_twiml_tracks_the_child_and_resumes_through_the_dial_action() {
        let id = Uuid::new_v4();
        let attempt = Uuid::new_v4();
        let xml = render_takeover_twiml("example.com", id, attempt, "+14155550101", "+14155550102");
        assert!(xml.contains("statusCallbackEvent=\"initiated ringing answered completed\""));
        assert!(xml.contains(&format!(
            "/twilio/takeover/finished?call_id={id}&amp;attempt={attempt}"
        )));
        assert!(xml.contains(&format!(
            "/twilio/takeover/status?call_id={id}&amp;attempt={attempt}"
        )));
        assert!(!xml.contains("<Redirect"));
        assert!(xml.contains("callerId=\"+14155550101\""));
    }
}
