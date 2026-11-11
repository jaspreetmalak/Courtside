use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use shared::{
    CallControlResponse, CallDetailResponse, CallDirection, CallMode, CallStatus,
    ListCallsResponse, StartCallRequest, StartCallResponse,
};
use uuid::Uuid;

use super::auth::AuthedUser;
use crate::db::entities::users;
use crate::state::AppState;

pub(crate) type ApiError = (StatusCode, String);

fn internal(err: impl std::fmt::Display) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}

pub async fn start_call(
    State(state): State<AppState>,
    user: AuthedUser,
    Json(request): Json<StartCallRequest>,
) -> Result<Json<StartCallResponse>, ApiError> {
    start_call_for_user(&state, &user.0, request)
        .await
        .map(Json)
}

/// Shared call-start workflow used by both the dedicated form and the chat
/// assistant. Keeping the action here ensures chat cannot bypass account,
/// number-pool, metering, destination, or Twilio checks.
pub(crate) async fn start_call_for_user(
    state: &AppState,
    user: &users::Model,
    mut request: StartCallRequest,
) -> Result<StartCallResponse, ApiError> {
    request.to = normalize_nanp_destination(&request.to).ok_or((
        StatusCode::BAD_REQUEST,
        "That phone number is incomplete or invalid. Enter 10 digits, or +1 followed by all 10 digits."
            .into(),
    ))?;
    if !valid_nanp_destination(&request.to) {
        return Err((
            StatusCode::BAD_REQUEST,
            "Free and paid calls currently support valid US/Canada numbers in +1 E.164 form."
                .into(),
        ));
    }
    let task = request.task.trim().to_string();
    if task.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Describe what Courtside should handle on the call.".into(),
        ));
    }
    if disallowed_call_task(&task) {
        return Err((
            StatusCode::BAD_REQUEST,
            "Courtside cannot place prank, harassing, threatening, deceptive, or impersonation calls."
                .into(),
        ));
    }
    let _start_guard = state.call_start.lock().await;
    let account = state.account_status(user.id).await.map_err(internal)?;
    if !account.phone_verified {
        return Err((
            StatusCode::FORBIDDEN,
            "Verify your mobile number first.".into(),
        ));
    }
    if account.app_attest_required && !account.device_attested {
        return Err((
            StatusCode::FORBIDDEN,
            "Verify this Apple device first.".into(),
        ));
    }
    if crate::db::has_active_call(&state.db, user.id)
        .await
        .map_err(internal)?
    {
        return Err((
            StatusCode::CONFLICT,
            "This account already has an active call.".into(),
        ));
    }
    let verified_phone = crate::db::verified_phone_for_user(&state.db, user.id)
        .await
        .map_err(internal)?
        .and_then(|phone| phone.caller_id_number)
        .ok_or((
            StatusCode::FORBIDDEN,
            "Verify your mobile caller ID before placing a call.".into(),
        ))?;
    let (bucket, remaining) = match account.tier {
        shared::AccountTier::Paid => (
            "paid",
            account.included_seconds_limit - account.included_seconds_used,
        ),
        shared::AccountTier::Free => (
            "trial",
            account.trial_seconds_limit - account.trial_seconds_used,
        ),
    };
    if remaining <= 0 {
        return Err((
            StatusCode::PAYMENT_REQUIRED,
            "Your included minutes are used.".into(),
        ));
    }
    let decision_plan = match crate::bridge::openai::plan_outbound_call(
        &state.config,
        &request.to,
        &user.display_name,
        user.announce_courtside,
        user.custom_call_instructions.as_deref(),
        &task,
    )
    .await
    .map_err(|err| {
        tracing::error!(?err, "failed to prepare call decision plan");
        (
            StatusCode::BAD_GATEWAY,
            "I couldn't prepare this call right now. Please try again.".into(),
        )
    })? {
        crate::bridge::openai::OutboundCallPlan::Ready {
            instructions,
            announce_courtside,
            share_user_name,
        } => (instructions, announce_courtside, share_user_name),
        crate::bridge::openai::OutboundCallPlan::NeedsUser(question) => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("Before I call, {question}"),
            ));
        }
    };
    let (decision_plan, announce_courtside, share_user_name) = decision_plan;
    let call = crate::db::create_metered_call(
        &state.db,
        user.id,
        CallDirection::Outbound,
        &request.to,
        Some(&decision_plan),
        None,
        (!state.config.dev_bypass_usage_limits).then_some(bucket),
        (!state.config.dev_bypass_usage_limits).then_some(remaining),
        announce_courtside,
        share_user_name,
        shared::CallAiBackend::OpenAiLive,
    )
    .await
    .map_err(internal)?;

    match crate::twilio::rest::place_outbound_call(
        &state.config,
        call.id,
        &verified_phone,
        &request.to,
        remaining,
    )
    .await
    {
        Ok(sid) => {
            crate::db::set_call_sid(&state.db, call.id, &sid)
                .await
                .map_err(internal)?;
            crate::twilio::rest::monitor_outbound_call(state.clone(), call.id, sid);
        }
        Err(err) => {
            tracing::error!(?err, call_id = %call.id, "failed to place outbound call");
            state
                .set_status_and_broadcast(call.id, CallStatus::Failed)
                .await;
            let _ = crate::db::finalize_call_usage(&state.db, call.id, 0).await;
            return Err((
                StatusCode::BAD_GATEWAY,
                "Twilio could not start the call. Check the phone number and try again.".into(),
            ));
        }
    }

    Ok(StartCallResponse { call })
}

pub(crate) fn normalize_nanp_destination(number: &str) -> Option<String> {
    let number = number.trim();
    if number.is_empty()
        || number.chars().any(|character| {
            !character.is_ascii_digit() && !matches!(character, '+' | ' ' | '-' | '.' | '(' | ')')
        })
        || number.chars().skip(1).any(|character| character == '+')
    {
        return None;
    }
    let has_explicit_country_code = number.starts_with('+');
    let digits = number
        .chars()
        .filter(char::is_ascii_digit)
        .collect::<String>();
    match digits.len() {
        10 if !has_explicit_country_code => Some(format!("+1{digits}")),
        11 if digits.starts_with('1') => Some(format!("+{digits}")),
        _ => None,
    }
}

fn disallowed_call_task(task: &str) -> bool {
    let task = task.to_ascii_lowercase();
    [
        "prank",
        "harass",
        "threaten",
        "scare them",
        "spam them",
        "impersonate",
        "pretend to be",
        "deceive them",
        "trick them",
        "brainrot",
    ]
    .iter()
    .any(|phrase| task.contains(phrase))
}

fn valid_nanp_destination(number: &str) -> bool {
    if number.len() != 12
        || !number.starts_with("+1")
        || !number[2..].chars().all(|c| c.is_ascii_digit())
    {
        return false;
    }
    let area = &number[2..5];
    let exchange = &number[5..8];
    let starts_with_subscriber_digit = |part: &str| matches!(part.as_bytes()[0], b'2'..=b'9');
    // Premium-rate and N11 service destinations are intentionally excluded.
    starts_with_subscriber_digit(area)
        && starts_with_subscriber_digit(exchange)
        && area != "900"
        && !exchange.ends_with("11")
        && !area.ends_with("11")
}

#[cfg(test)]
mod tests {
    use super::{disallowed_call_task, normalize_nanp_destination, valid_nanp_destination};

    #[test]
    fn accepts_us_and_canada_e164_and_blocks_special_numbers() {
        assert!(valid_nanp_destination("+14155551212"));
        assert!(valid_nanp_destination("+16045551212"));
        assert!(!valid_nanp_destination("+442079460000"));
        assert!(!valid_nanp_destination("+19005551212"));
        assert!(!valid_nanp_destination("+14115551212"));
        assert!(!valid_nanp_destination("+11530555012"));
        assert!(!valid_nanp_destination("+14150551212"));
    }

    #[test]
    fn normalizes_common_north_american_number_formats() {
        assert_eq!(
            normalize_nanp_destination("(415) 555-1212").as_deref(),
            Some("+14155551212")
        );
        assert_eq!(
            normalize_nanp_destination("1 604 555 1212").as_deref(),
            Some("+16045551212")
        );
        assert_eq!(
            normalize_nanp_destination("+1 (604) 555-1212").as_deref(),
            Some("+16045551212")
        );
        assert_eq!(normalize_nanp_destination("+1530216831"), None);
        assert_eq!(normalize_nanp_destination("415+5551212"), None);
        assert_eq!(normalize_nanp_destination("call 4155551212"), None);
    }

    #[test]
    fn blocks_abusive_call_objectives_without_blocking_legitimate_reports() {
        assert!(disallowed_call_task("Prank him and play noises"));
        assert!(disallowed_call_task("Just brainrot him"));
        assert!(!disallowed_call_task(
            "Call my bank's fraud department to report a scam"
        ));
    }
}

pub async fn list_calls(
    State(state): State<AppState>,
    user: AuthedUser,
) -> Result<Json<ListCallsResponse>, ApiError> {
    let calls = crate::db::list_calls(&state.db, user.0.id)
        .await
        .map_err(internal)?;
    Ok(Json(ListCallsResponse { calls }))
}

pub async fn call_detail(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(id): Path<Uuid>,
) -> Result<Json<CallDetailResponse>, ApiError> {
    let call = crate::db::find_call_for_user(&state.db, id, user.0.id)
        .await
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "call not found".to_string()))?;
    let transcript = crate::db::call_transcript(&state.db, id)
        .await
        .map_err(internal)?;
    let control = state.call_control(id).await;
    Ok(Json(CallDetailResponse {
        call: crate::db::call_to_shared(&call),
        transcript,
        mode: control.mode,
        control_message: control.message,
        chat_id: call.chat_id,
        handoff: state.call_handoff(id).await,
        pending_question: state.call_question(id).await,
        remembering: state.call_remembering(id).await,
    }))
}

pub async fn decline_call_handoff(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(id): Path<Uuid>,
    Json(request): Json<shared::DeclineCallHandoffRequest>,
) -> Result<Json<CallControlResponse>, ApiError> {
    active_call_for_user(&state, id, user.0.id).await?;
    let relay = state.voice_relay(id).await.ok_or((
        StatusCode::CONFLICT,
        "That call is no longer available.".into(),
    ))?;
    let (reply, declined) = tokio::sync::oneshot::channel();
    relay
        .send(crate::bridge::voice::VoiceCommand::DeclineHandoff {
            request_id: request.request_id,
            reply,
        })
        .await
        .map_err(|_| (StatusCode::CONFLICT, "That call has already ended.".into()))?;
    match tokio::time::timeout(std::time::Duration::from_secs(5), declined).await {
        Ok(Ok(Ok(()))) => Ok(Json(state.call_control(id).await)),
        Ok(Ok(Err(message))) => Err((StatusCode::CONFLICT, message)),
        _ => Err((
            StatusCode::GATEWAY_TIMEOUT,
            "Courtside is still handling the call. Please try again.".into(),
        )),
    }
}

pub async fn end_call(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(id): Path<Uuid>,
) -> Result<Json<CallControlResponse>, ApiError> {
    let control_lock = state.call_control_lock(id).await;
    let _control_guard = control_lock.lock().await;
    let call = active_call_for_user(&state, id, user.0.id).await?;
    let sid = call.twilio_call_sid.ok_or((
        StatusCode::CONFLICT,
        "The call has not reached Twilio yet.".into(),
    ))?;
    crate::twilio::rest::hangup_call(&state.config, &sid)
        .await
        .map_err(|err| {
            tracing::error!(?err, %id, "manual call hangup failed");
            (
                StatusCode::BAD_GATEWAY,
                "I couldn't end the call yet. Try again.".into(),
            )
        })?;
    Ok(Json(state.call_control(id).await))
}

pub async fn takeover_call(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(id): Path<Uuid>,
) -> Result<Json<CallControlResponse>, ApiError> {
    let control_lock = state.call_control_lock(id).await;
    let _control_guard = control_lock.lock().await;
    let call = active_call_for_user(&state, id, user.0.id).await?;
    if call.status != "in_progress" {
        return Err((
            StatusCode::CONFLICT,
            "The call hasn't connected yet. Take over once the other person answers.".into(),
        ));
    }
    if state.call_mode(id).await != CallMode::Assistant {
        return Ok(Json(state.call_control(id).await));
    }
    let verified_phone = crate::db::verified_phone_for_user(&state.db, user.0.id)
        .await
        .map_err(internal)?
        .and_then(|phone| phone.caller_id_number);
    let takeover_phone = user
        .0
        .takeover_phone_number
        .as_deref()
        .filter(|number| !number.is_empty())
        .or(verified_phone.as_deref())
        .ok_or((
            StatusCode::CONFLICT,
            "Add your takeover phone number in Account first.".into(),
        ))?;
    if call.peer_number == takeover_phone {
        return Err((
            StatusCode::CONFLICT,
            "The takeover number must be different from the person being called.".into(),
        ));
    }
    let sid = call.twilio_call_sid.as_deref().ok_or((
        StatusCode::CONFLICT,
        "The call has not reached Twilio yet.".into(),
    ))?;
    // Ring the owner's phone from their Courtside number, rather than making
    // their own verified mobile number appear to call itself.
    let owned_number = crate::db::owned_number_for_user(&state.db, user.0.id)
        .await
        .map_err(internal)?;
    let from_number = owned_number
        .as_ref()
        .map(|number| number.phone_number.as_str())
        .or(verified_phone
            .as_deref()
            .filter(|number| *number != takeover_phone))
        .ok_or((
            StatusCode::CONFLICT,
            "A Courtside caller ID is needed to ring your phone.".into(),
        ))?;
    let attempt = state.begin_takeover(id).await;
    let twiml =
        crate::twilio::webhook::takeover_twiml(&state, id, attempt, from_number, takeover_phone);
    if let Err(err) = crate::twilio::rest::redirect_call_to_twiml(&state.config, sid, &twiml).await
    {
        state
            .update_takeover(
                id,
                attempt,
                CallMode::Assistant,
                "Takeover couldn't start. Courtside still has the call.",
            )
            .await;
        tracing::error!(?err, %id, "takeover redirect failed");
        return Err((
            StatusCode::BAD_GATEWAY,
            "Twilio couldn't connect you to the call. Try again.".into(),
        ));
    }
    tracing::info!(%id, %attempt, "takeover dialing requested");
    Ok(Json(state.call_control(id).await))
}

pub async fn resume_assistant(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(id): Path<Uuid>,
) -> Result<Json<CallControlResponse>, ApiError> {
    let control_lock = state.call_control_lock(id).await;
    let _control_guard = control_lock.lock().await;
    let call = active_call_for_user(&state, id, user.0.id).await?;
    if state.call_mode(id).await == CallMode::InAppTakeover {
        // The media loop needs this lock to publish the restored state. Do
        // not hold it while waiting for that loop's acknowledgement.
        drop(_control_guard);
        let relay = state.voice_relay(id).await.ok_or((
            StatusCode::CONFLICT,
            "Call audio is reconnecting. Try again in a moment.".into(),
        ))?;
        let (reply, returned) = tokio::sync::oneshot::channel();
        relay
            .send(crate::bridge::voice::VoiceCommand::Leave {
                session: None,
                reply: Some(reply),
            })
            .await
            .map_err(|_| (StatusCode::CONFLICT, "That call has already ended.".into()))?;
        match tokio::time::timeout(std::time::Duration::from_secs(25), returned).await {
            Ok(Ok(Ok(()))) => return Ok(Json(state.call_control(id).await)),
            Ok(Ok(Err(message))) => return Err((StatusCode::BAD_GATEWAY, message)),
            _ => {
                return Err((
                    StatusCode::BAD_GATEWAY,
                    "Courtside is reconnecting to the call. Please wait.".into(),
                ))
            }
        }
    }
    let sid = call.twilio_call_sid.as_deref().ok_or((
        StatusCode::CONFLICT,
        "The call has not reached Twilio yet.".into(),
    ))?;
    let attempt = state.takeover_attempt(id).await;
    let twiml = crate::twilio::webhook::assistant_resume_twiml(&state, id, attempt);
    if let Err(err) = crate::twilio::rest::redirect_call_to_twiml(&state.config, sid, &twiml).await
    {
        tracing::error!(?err, %id, "assistant resume redirect failed");
        return Err((
            StatusCode::BAD_GATEWAY,
            "Twilio couldn't return the call to Courtside. Try again.".into(),
        ));
    }
    state
        .set_call_control(id, CallMode::Assistant, "Courtside has the call again.")
        .await;
    Ok(Json(state.call_control(id).await))
}

async fn active_call_for_user(
    state: &AppState,
    call_id: Uuid,
    user_id: Uuid,
) -> Result<crate::db::entities::calls::Model, ApiError> {
    let call = crate::db::find_call_for_user(&state.db, call_id, user_id)
        .await
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "call not found".into()))?;
    if !matches!(call.status.as_str(), "queued" | "ringing" | "in_progress") {
        return Err((StatusCode::CONFLICT, "That call has already ended.".into()));
    }
    Ok(call)
}
