use std::time::Duration;

use axum::{extract::State, http::StatusCode, Json};
use serde_json::Value;
use sha2::{Digest, Sha256};
use shared::{
    AccountStatusResponse, PhoneVerificationConfirmRequest, PhoneVerificationStartRequest,
    PhoneVerificationStartResponse,
};

use super::auth::AuthedUser;
use crate::state::{AppState, PendingCallerIdVerification};

type ApiError = (StatusCode, String);

const CHALLENGE_TTL: Duration = Duration::from_secs(10 * 60);
const START_COOLDOWN: Duration = Duration::from_secs(60);

fn valid_e164(value: &str) -> bool {
    value.starts_with('+')
        && (8..=16).contains(&value.len())
        && value[1..].chars().all(|c| c.is_ascii_digit())
}

fn phone_hash(state: &AppState, phone_number: &str) -> String {
    hex::encode(Sha256::digest(
        format!("{}:{phone_number}", state.config.phone_hash_secret).as_bytes(),
    ))
}

fn masked_phone(phone_number: &str) -> String {
    let suffix = phone_number.chars().rev().take(4).collect::<String>();
    format!("•••-•••-{}", suffix.chars().rev().collect::<String>())
}

fn validation_code(body: &Value) -> Option<String> {
    body["validation_code"]
        .as_str()
        .map(str::to_string)
        .or_else(|| {
            body["validation_code"]
                .as_i64()
                .map(|code| code.to_string())
        })
}

fn twilio_error(status: reqwest::StatusCode, body: &Value) -> ApiError {
    if status == reqwest::StatusCode::UNAUTHORIZED || body["code"].as_i64() == Some(20003) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Twilio rejected the caller-ID credentials. Check the server API Key SID and API Secret."
                .into(),
        );
    }
    let message = body["message"]
        .as_str()
        .unwrap_or("Twilio caller-ID verification failed");
    (
        if status.is_client_error() {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::BAD_GATEWAY
        },
        message.into(),
    )
}

fn voice_api_url(state: &AppState, suffix: &str) -> String {
    format!(
        "https://api.twilio.com/2010-04-01/Accounts/{}/{}",
        state.config.twilio_account_sid, suffix
    )
}

/// Start Twilio's real OutgoingCallerId verification. Twilio calls the number
/// after a short delay and asks the owner to enter the returned validation code.
pub async fn start(
    State(state): State<AppState>,
    user: AuthedUser,
    Json(request): Json<PhoneVerificationStartRequest>,
) -> Result<Json<PhoneVerificationStartResponse>, ApiError> {
    require_eligible_device(&state, user.0.id).await?;
    let phone_number = request.phone_number.trim();
    if !valid_e164(phone_number) {
        return Err((
            StatusCode::BAD_REQUEST,
            "Enter a phone number in E.164 form, such as +15305550123.".into(),
        ));
    }

    let hash = phone_hash(&state, phone_number);
    if let Some(existing) = crate::db::verified_phone_by_hash(&state.db, &hash)
        .await
        .map_err(internal)?
    {
        if existing.user_id != user.0.id {
            return Err((
                StatusCode::CONFLICT,
                "That phone number is already attached to another Courtside account.".into(),
            ));
        }
        if existing.caller_id_number.as_deref() == Some(phone_number)
            && existing.outgoing_caller_id_sid.is_some()
        {
            return Err((
                StatusCode::CONFLICT,
                "That number is already your verified caller ID.".into(),
            ));
        }
    }

    {
        let pending = state.caller_id_verifications.lock().await;
        if pending
            .get(&user.0.id)
            .is_some_and(|challenge| challenge.started_at.elapsed() < START_COOLDOWN)
        {
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                "Wait one minute before requesting another verification call.".into(),
            ));
        }
    }

    let response = reqwest::Client::new()
        .post(voice_api_url(&state, "OutgoingCallerIds.json"))
        .basic_auth(
            &state.config.twilio_api_key_sid,
            Some(&state.config.twilio_api_secret),
        )
        .form(&[
            ("PhoneNumber", phone_number),
            ("FriendlyName", "Courtside user caller ID"),
            // Give the app enough time to render the code before the phone rings.
            ("CallDelay", "6"),
        ])
        .send()
        .await
        .map_err(|err| (StatusCode::BAD_GATEWAY, err.to_string()))?;
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .map_err(|err| (StatusCode::BAD_GATEWAY, err.to_string()))?;
    if !status.is_success() {
        return Err(twilio_error(status, &body));
    }
    let validation_code = validation_code(&body).ok_or((
        StatusCode::BAD_GATEWAY,
        "Twilio did not return a caller-ID validation code.".into(),
    ))?;

    state.caller_id_verifications.lock().await.insert(
        user.0.id,
        PendingCallerIdVerification {
            phone_number: phone_number.to_string(),
            validation_code: validation_code.clone(),
            started_at: std::time::Instant::now(),
        },
    );

    Ok(Json(PhoneVerificationStartResponse {
        sent: true,
        validation_code,
    }))
}

/// Confirm that Twilio now lists the pending number as an authorized outgoing
/// caller ID, then persist it as this account's only outbound `From` number.
pub async fn confirm(
    State(state): State<AppState>,
    user: AuthedUser,
    Json(request): Json<PhoneVerificationConfirmRequest>,
) -> Result<Json<AccountStatusResponse>, ApiError> {
    require_eligible_device(&state, user.0.id).await?;
    let phone_number = request.phone_number.trim();
    if !valid_e164(phone_number)
        || request.code.len() < 4
        || !request.code.chars().all(|c| c.is_ascii_digit())
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "Invalid phone number or caller-ID code.".into(),
        ));
    }

    let pending = state
        .caller_id_verifications
        .lock()
        .await
        .get(&user.0.id)
        .cloned()
        .ok_or((
            StatusCode::CONFLICT,
            "Start a new caller-ID verification first.".into(),
        ))?;
    if pending.started_at.elapsed() > CHALLENGE_TTL {
        state
            .caller_id_verifications
            .lock()
            .await
            .remove(&user.0.id);
        return Err((
            StatusCode::CONFLICT,
            "That verification expired. Start a new verification call.".into(),
        ));
    }
    if pending.phone_number != phone_number || pending.validation_code != request.code {
        return Err((
            StatusCode::BAD_REQUEST,
            "The phone number or validation code does not match this verification.".into(),
        ));
    }

    let list_url = format!(
        "{}?PhoneNumber={}",
        voice_api_url(&state, "OutgoingCallerIds.json"),
        urlencoding::encode(phone_number)
    );
    let response = reqwest::Client::new()
        .get(list_url)
        .basic_auth(
            &state.config.twilio_api_key_sid,
            Some(&state.config.twilio_api_secret),
        )
        .send()
        .await
        .map_err(|err| (StatusCode::BAD_GATEWAY, err.to_string()))?;
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .map_err(|err| (StatusCode::BAD_GATEWAY, err.to_string()))?;
    if !status.is_success() {
        return Err(twilio_error(status, &body));
    }
    let caller_id = body["outgoing_caller_ids"]
        .as_array()
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry["phone_number"].as_str() == Some(phone_number))
        })
        .ok_or((
            StatusCode::CONFLICT,
            "Twilio has not verified the number yet. Enter the code during the verification call, then check again."
                .into(),
        ))?;
    let outgoing_caller_id_sid = caller_id["sid"].as_str().ok_or((
        StatusCode::BAD_GATEWAY,
        "Twilio returned a caller ID without its identifier.".into(),
    ))?;

    // Lookup keeps the one-trial-per-mobile rule while the caller-ID challenge
    // separately authorizes the number for outbound voice calls.
    let lookup_url = format!(
        "https://lookups.twilio.com/v2/PhoneNumbers/{}?Fields=line_type_intelligence",
        urlencoding::encode(phone_number)
    );
    let lookup = reqwest::Client::new()
        .get(lookup_url)
        .basic_auth(
            &state.config.twilio_api_key_sid,
            Some(&state.config.twilio_api_secret),
        )
        .send()
        .await
        .map_err(|err| (StatusCode::BAD_GATEWAY, err.to_string()))?;
    let lookup_status = lookup.status();
    let lookup_body: Value = lookup
        .json()
        .await
        .map_err(|err| (StatusCode::BAD_GATEWAY, err.to_string()))?;
    if !lookup_status.is_success() {
        return Err(twilio_error(lookup_status, &lookup_body));
    }
    let line_type = lookup_body["line_type_intelligence"]["type"]
        .as_str()
        .unwrap_or("unknown");
    if line_type != "mobile" {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            "Courtside currently requires a mobile caller ID; VoIP and landline numbers are not eligible."
                .into(),
        ));
    }

    let hash = phone_hash(&state, phone_number);
    let country = lookup_body["country_code"].as_str().unwrap_or("US");
    crate::db::save_verified_phone(
        &state.db,
        user.0.id,
        &hash,
        &masked_phone(phone_number),
        line_type,
        country,
        phone_number,
        outgoing_caller_id_sid,
    )
    .await
    .map_err(|err| {
        let message = if err.to_string().contains("UNIQUE") {
            "That mobile number has already received a Courtside trial.".into()
        } else {
            err.to_string()
        };
        (StatusCode::CONFLICT, message)
    })?;

    state
        .caller_id_verifications
        .lock()
        .await
        .remove(&user.0.id);
    if state.config.require_app_attest {
        crate::api::device::mark_trial_used(&state, user.0.id).await?;
    }

    state
        .account_status(user.0.id)
        .await
        .map(Json)
        .map_err(internal)
}

fn internal(err: impl std::fmt::Display) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}

async fn require_eligible_device(state: &AppState, user_id: uuid::Uuid) -> Result<(), ApiError> {
    if !state.config.require_app_attest {
        return Ok(());
    }
    let device = crate::db::device_attestation_for_user(&state.db, user_id)
        .await
        .map_err(internal)?
        .ok_or((
            StatusCode::FORBIDDEN,
            "Complete Apple device verification first.".into(),
        ))?;
    if device.trial_previously_used {
        return Err((
            StatusCode::CONFLICT,
            "This Apple device has already received a Courtside trial.".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{masked_phone, valid_e164, validation_code};
    use serde_json::json;

    #[test]
    fn caller_id_inputs_are_strict_and_codes_accept_twilio_number_values() {
        assert!(valid_e164("+15305550123"));
        assert!(!valid_e164("530-555-0123"));
        assert!(!valid_e164("+1 (530) 555-0123"));
        assert_eq!(masked_phone("+15305550123"), "•••-•••-0123");
        assert_eq!(
            validation_code(&json!({"validation_code": 123456})).as_deref(),
            Some("123456")
        );
        assert_eq!(
            validation_code(&json!({"validation_code": "654321"})).as_deref(),
            Some("654321")
        );
    }
}
