//! Paid dedicated-number claims and the shared outbound-only trial pool.
//!
//! Purchasing charges the connected Twilio account (typically ~$1/month for a
//! US local number) — the client shows that before confirming. A purchased
//! number's voice webhook is pointed at this server automatically.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{Duration, Utc};
use serde::Deserialize;
use shared::{
    AvailableNumber, AvailableNumbersResponse, OwnedNumber, OwnedNumbersResponse,
    PurchaseNumberRequest,
};

use super::auth::AuthedUser;
use crate::state::AppState;

type ApiError = (StatusCode, String);

fn internal(err: impl std::fmt::Display) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}

fn twilio_failure(
    operation: &str,
    status: reqwest::StatusCode,
    body: &serde_json::Value,
) -> ApiError {
    if status == reqwest::StatusCode::UNAUTHORIZED || body["code"].as_i64() == Some(20003) {
        return (
            StatusCode::BAD_GATEWAY,
            "Twilio number service is not configured. Check the server API Key SID and API Secret."
                .into(),
        );
    }
    (
        StatusCode::BAD_GATEWAY,
        format!(
            "{operation}: {}",
            body["message"].as_str().unwrap_or("Twilio request failed")
        ),
    )
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    /// 3-digit US area code, e.g. "415".
    pub area_code: Option<String>,
}

pub async fn search(
    State(state): State<AppState>,
    user: AuthedUser,
    Query(query): Query<SearchQuery>,
) -> Result<Json<AvailableNumbersResponse>, ApiError> {
    require_paid(&state, user.0.id).await?;
    if crate::db::owned_number_for_user(&state.db, user.0.id)
        .await
        .map_err(internal)?
        .is_some()
    {
        return Err((
            StatusCode::CONFLICT,
            "this account already has a dedicated number".into(),
        ));
    }

    let mut url = format!(
        "https://api.twilio.com/2010-04-01/Accounts/{}/AvailablePhoneNumbers/US/Local.json?VoiceEnabled=true&PageSize=10",
        state.config.twilio_account_sid
    );
    if let Some(area_code) = query.area_code.as_deref().filter(|s| !s.is_empty()) {
        if area_code.len() != 3 || !area_code.chars().all(|c| c.is_ascii_digit()) {
            return Err((StatusCode::BAD_REQUEST, "area code must be 3 digits".into()));
        }
        url.push_str(&format!("&AreaCode={area_code}"));
    }

    let body = twilio_get(&state, &url).await?;
    let numbers = body["available_phone_numbers"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| AvailableNumber {
                    phone_number: entry["phone_number"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    friendly_name: entry["friendly_name"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    locality: entry["locality"].as_str().map(str::to_string),
                    region: entry["region"].as_str().map(str::to_string),
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Json(AvailableNumbersResponse { numbers }))
}

pub async fn purchase(
    State(state): State<AppState>,
    user: AuthedUser,
    Json(request): Json<PurchaseNumberRequest>,
) -> Result<Json<OwnedNumber>, ApiError> {
    require_paid(&state, user.0.id).await?;
    if crate::db::owned_number_for_user(&state.db, user.0.id)
        .await
        .map_err(internal)?
        .is_some()
    {
        return Err((
            StatusCode::CONFLICT,
            "this account already has a dedicated number".into(),
        ));
    }
    if !request.phone_number.starts_with('+')
        || !request.phone_number[1..]
            .chars()
            .all(|c| c.is_ascii_digit())
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "phone number must use E.164 format".into(),
        ));
    }

    let purchased = purchase_twilio_number(&state, Some(&request.phone_number)).await?;

    let assigned = match crate::db::assign_owned_number(
        &state.db,
        user.0.id,
        &purchased.twilio_sid,
        &purchased.phone_number,
        &purchased.friendly_name,
    )
    .await
    {
        Ok(number) => number,
        Err(err) => {
            // A concurrent request may have claimed another number after our
            // initial check. Release this purchase so the account is not billed
            // for an orphaned Twilio number.
            if let Err(release_err) = release_twilio_number(&state, &purchased.twilio_sid).await {
                tracing::error!(?release_err, twilio_sid = %purchased.twilio_sid, "failed to release orphaned number");
            }
            return Err(internal(err));
        }
    };

    Ok(Json(OwnedNumber {
        phone_number: assigned.phone_number,
        friendly_name: assigned.friendly_name,
        inbound_enabled: true,
    }))
}

pub async fn owned(
    State(state): State<AppState>,
    user: AuthedUser,
) -> Result<Json<OwnedNumbersResponse>, ApiError> {
    let numbers = crate::db::owned_number_for_user(&state.db, user.0.id)
        .await
        .map_err(internal)?
        .into_iter()
        .map(|number| OwnedNumber {
            phone_number: number.phone_number,
            friendly_name: number.friendly_name,
            inbound_enabled: true,
        })
        .collect();
    Ok(Json(OwnedNumbersResponse { numbers }))
}

pub async fn release_twilio_number(state: &AppState, twilio_sid: &str) -> Result<(), ApiError> {
    let url = format!(
        "https://api.twilio.com/2010-04-01/Accounts/{}/IncomingPhoneNumbers/{twilio_sid}.json",
        state.config.twilio_account_sid
    );
    let response = reqwest::Client::new()
        .delete(url)
        .basic_auth(
            &state.config.twilio_api_key_sid,
            Some(&state.config.twilio_api_secret),
        )
        .send()
        .await
        .map_err(internal)?;
    if !response.status().is_success() {
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("Twilio release failed ({})", response.status()),
        ));
    }
    Ok(())
}

/// Refresh inbound Voice URLs after a local ngrok hostname changes.
pub async fn sync_voice_webhooks(state: &AppState) -> anyhow::Result<()> {
    let numbers = crate::db::all_active_numbers(&state.db).await?;
    if numbers.is_empty() {
        return Ok(());
    }

    let voice_url = format!("https://{}/twilio/voice", state.config.public_host);
    let client = reqwest::Client::new();
    for number in numbers {
        let url = format!(
            "https://api.twilio.com/2010-04-01/Accounts/{}/IncomingPhoneNumbers/{}.json",
            state.config.twilio_account_sid, number.twilio_sid
        );
        let response = client
            .post(url)
            .basic_auth(
                &state.config.twilio_api_key_sid,
                Some(&state.config.twilio_api_secret),
            )
            .form(&[("VoiceUrl", voice_url.as_str()), ("VoiceMethod", "POST")])
            .send()
            .await?;
        if !response.status().is_success() {
            anyhow::bail!(
                "Twilio rejected webhook update for {} ({})",
                number.phone_number,
                response.status()
            );
        }
    }
    Ok(())
}

#[derive(Debug)]
struct TwilioPurchase {
    twilio_sid: String,
    phone_number: String,
    friendly_name: String,
}

async fn purchase_twilio_number(
    state: &AppState,
    requested: Option<&str>,
) -> Result<TwilioPurchase, ApiError> {
    let phone_number = match requested {
        Some(number) => number.to_string(),
        None => {
            let url = format!(
                "https://api.twilio.com/2010-04-01/Accounts/{}/AvailablePhoneNumbers/US/Local.json?VoiceEnabled=true&PageSize=1",
                state.config.twilio_account_sid
            );
            let body = twilio_get(state, &url).await?;
            body["available_phone_numbers"]
                .as_array()
                .and_then(|numbers| numbers.first())
                .and_then(|number| number["phone_number"].as_str())
                .map(str::to_string)
                .ok_or((
                    StatusCode::SERVICE_UNAVAILABLE,
                    "No trial numbers are available.".into(),
                ))?
        }
    };
    let url = format!(
        "https://api.twilio.com/2010-04-01/Accounts/{}/IncomingPhoneNumbers.json",
        state.config.twilio_account_sid
    );
    let voice_url = format!("https://{}/twilio/voice", state.config.public_host);
    let response = reqwest::Client::new()
        .post(url)
        .basic_auth(
            &state.config.twilio_api_key_sid,
            Some(&state.config.twilio_api_secret),
        )
        .form(&[
            ("PhoneNumber", phone_number.as_str()),
            ("VoiceUrl", voice_url.as_str()),
            ("VoiceMethod", "POST"),
        ])
        .send()
        .await
        .map_err(internal)?;
    let status = response.status();
    let body: serde_json::Value = response.json().await.map_err(internal)?;
    if !status.is_success() {
        return Err(twilio_failure("Number purchase failed", status, &body));
    }
    let twilio_sid = body["sid"].as_str().ok_or((
        StatusCode::BAD_GATEWAY,
        "Twilio response missing number SID".into(),
    ))?;
    let phone_number = body["phone_number"].as_str().ok_or((
        StatusCode::BAD_GATEWAY,
        "Twilio response missing phone number".into(),
    ))?;
    Ok(TwilioPurchase {
        twilio_sid: twilio_sid.into(),
        phone_number: phone_number.into(),
        friendly_name: body["friendly_name"]
            .as_str()
            .unwrap_or(phone_number)
            .into(),
    })
}

/// Returns a reusable outbound caller ID, purchasing at most one trial-pool
/// number per UTC day and never keeping more than three.
pub async fn ensure_trial_pool_number(
    state: &AppState,
) -> Result<crate::db::entities::phone_numbers::Model, ApiError> {
    if let Some(number) = crate::db::available_pool_number(&state.db)
        .await
        .map_err(internal)?
    {
        return Ok(number);
    }
    let _guard = state.number_provisioning.lock().await;
    if let Some(number) = crate::db::available_pool_number(&state.db)
        .await
        .map_err(internal)?
    {
        return Ok(number);
    }
    if crate::db::active_pool_count(&state.db)
        .await
        .map_err(internal)?
        >= 3
    {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "Trial number pool is busy.".into(),
        ));
    }
    let since = Utc::now() - Duration::hours(24);
    if crate::db::pool_purchases_since(&state.db, since)
        .await
        .map_err(internal)?
        >= 1
    {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "Trial number inventory is temporarily unavailable. Please try again later.".into(),
        ));
    }
    let purchased = purchase_twilio_number(state, None).await?;
    match crate::db::create_pool_number(
        &state.db,
        &purchased.twilio_sid,
        &purchased.phone_number,
        &purchased.friendly_name,
        Utc::now() + Duration::days(30),
    )
    .await
    {
        Ok(number) => Ok(number),
        Err(err) => {
            if let Err(release_err) = release_twilio_number(state, &purchased.twilio_sid).await {
                tracing::error!(?release_err, "failed to release orphaned pool number");
            }
            Err(internal(err))
        }
    }
}

async fn require_paid(state: &AppState, user_id: uuid::Uuid) -> Result<(), ApiError> {
    let status = state.account_status(user_id).await.map_err(internal)?;
    if !status.phone_verified {
        return Err((
            StatusCode::FORBIDDEN,
            "Verify your mobile number first.".into(),
        ));
    }
    if status.app_attest_required && !status.device_attested {
        return Err((
            StatusCode::FORBIDDEN,
            "Verify this Apple device first.".into(),
        ));
    }
    if status.tier != shared::AccountTier::Paid {
        return Err((
            StatusCode::PAYMENT_REQUIRED,
            "A paid subscription is required to choose a dedicated number.".into(),
        ));
    }
    Ok(())
}

async fn twilio_get(state: &AppState, url: &str) -> Result<serde_json::Value, ApiError> {
    let response = reqwest::Client::new()
        .get(url)
        .basic_auth(
            &state.config.twilio_api_key_sid,
            Some(&state.config.twilio_api_secret),
        )
        .send()
        .await
        .map_err(internal)?;
    let status = response.status();
    let body: serde_json::Value = response.json().await.map_err(internal)?;
    if !status.is_success() {
        return Err(twilio_failure("Number search failed", status, &body));
    }
    Ok(body)
}
