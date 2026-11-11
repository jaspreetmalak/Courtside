use std::fs;

use appattest::attestation::Attestation;
use axum::{extract::State, http::StatusCode, Json};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use chrono::Utc;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use shared::{AccountStatusResponse, AttestationChallengeResponse, DeviceAttestationRequest};

use super::auth::AuthedUser;
use crate::state::AppState;

type ApiError = (StatusCode, String);

#[derive(Serialize)]
struct DeviceCheckClaims<'a> {
    iss: &'a str,
    iat: i64,
}

#[derive(Serialize)]
struct DeviceCheckBody<'a> {
    device_token: &'a str,
    transaction_id: String,
    timestamp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    bit0: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bit1: Option<bool>,
}

#[derive(Deserialize, Default)]
struct DeviceCheckBits {
    #[serde(default)]
    bit0: bool,
}

fn devicecheck_jwt(state: &AppState) -> Result<String, ApiError> {
    let team_id = state.config.apple_team_id.as_deref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Set APPLE_TEAM_ID for DeviceCheck.".into(),
    ))?;
    let key_id = state.config.devicecheck_key_id.as_deref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Set DEVICECHECK_KEY_ID for DeviceCheck.".into(),
    ))?;
    let key_path = state
        .config
        .devicecheck_private_key_path
        .as_deref()
        .ok_or((
            StatusCode::SERVICE_UNAVAILABLE,
            "Set DEVICECHECK_PRIVATE_KEY_PATH for DeviceCheck.".into(),
        ))?;
    let pem =
        fs::read(key_path).map_err(|err| (StatusCode::SERVICE_UNAVAILABLE, err.to_string()))?;
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some(key_id.into());
    jsonwebtoken::encode(
        &header,
        &DeviceCheckClaims {
            iss: team_id,
            iat: Utc::now().timestamp(),
        },
        &EncodingKey::from_ec_pem(&pem)
            .map_err(|err| (StatusCode::SERVICE_UNAVAILABLE, err.to_string()))?,
    )
    .map_err(|err| (StatusCode::SERVICE_UNAVAILABLE, err.to_string()))
}

async fn devicecheck_request<T: for<'de> Deserialize<'de> + Default>(
    state: &AppState,
    operation: &str,
    token: &str,
    bit0: Option<bool>,
) -> Result<T, ApiError> {
    let response = reqwest::Client::new()
        .post(format!("https://api.devicecheck.apple.com/v1/{operation}"))
        .bearer_auth(devicecheck_jwt(state)?)
        .json(&DeviceCheckBody {
            device_token: token,
            transaction_id: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now().timestamp_millis(),
            bit0,
            bit1: bit0.map(|_| false),
        })
        .send()
        .await
        .map_err(|err| (StatusCode::BAD_GATEWAY, err.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        let message = response.text().await.unwrap_or_default();
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("Apple DeviceCheck failed ({status}): {message}"),
        ));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|err| (StatusCode::BAD_GATEWAY, err.to_string()))?;
    if bytes.is_empty() {
        return Ok(T::default());
    }
    serde_json::from_slice(&bytes).map_err(|err| (StatusCode::BAD_GATEWAY, err.to_string()))
}

async fn trial_was_used(state: &AppState, token: &str) -> Result<bool, ApiError> {
    Ok(
        devicecheck_request::<DeviceCheckBits>(state, "query_two_bits", token, None)
            .await?
            .bit0,
    )
}

pub async fn mark_trial_used(state: &AppState, user_id: uuid::Uuid) -> Result<(), ApiError> {
    let device = crate::db::device_attestation_for_user(&state.db, user_id)
        .await
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?
        .ok_or((
            StatusCode::FORBIDDEN,
            "Device attestation is required.".into(),
        ))?;
    devicecheck_request::<serde_json::Value>(
        state,
        "update_two_bits",
        &device.device_token,
        Some(true),
    )
    .await?;
    crate::db::mark_device_trial_used(&state.db, user_id)
        .await
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))
}

pub async fn challenge(
    State(state): State<AppState>,
    user: AuthedUser,
) -> Result<Json<AttestationChallengeResponse>, ApiError> {
    if !state.config.require_app_attest {
        return Err((StatusCode::NOT_FOUND, "App Attest is disabled.".into()));
    }
    let challenge = crate::db::create_attestation_challenge(&state.db, user.0.id)
        .await
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    Ok(Json(AttestationChallengeResponse { challenge }))
}

pub async fn attest(
    State(state): State<AppState>,
    user: AuthedUser,
    Json(request): Json<DeviceAttestationRequest>,
) -> Result<Json<AccountStatusResponse>, ApiError> {
    if !state.config.require_app_attest {
        return state
            .account_status(user.0.id)
            .await
            .map(Json)
            .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()));
    }
    if !crate::db::consume_attestation_challenge(&state.db, &request.challenge, user.0.id)
        .await
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?
    {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Attestation challenge expired.".into(),
        ));
    }
    if let Some(existing) = crate::db::device_attestation_by_key(&state.db, &request.key_id)
        .await
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?
    {
        if existing.user_id != user.0.id {
            return Err((
                StatusCode::CONFLICT,
                "This Apple device has already been linked to another Courtside trial.".into(),
            ));
        }
        return state
            .account_status(user.0.id)
            .await
            .map(Json)
            .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()));
    }
    let team_id = state.config.apple_team_id.as_deref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Set APPLE_TEAM_ID for App Attest.".into(),
    ))?;
    let root_path = state.config.app_attest_root_ca_path.as_deref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Set APP_ATTEST_ROOT_CA_PATH.".into(),
    ))?;
    let root =
        fs::read(root_path).map_err(|err| (StatusCode::SERVICE_UNAVAILABLE, err.to_string()))?;
    let cbor = Attestation::decode_base64(&request.attestation)
        .map_err(|_| (StatusCode::UNAUTHORIZED, "Invalid App Attest data.".into()))?;
    let attestation = Attestation::from_cbor_bytes(&cbor)
        .map_err(|_| (StatusCode::UNAUTHORIZED, "Invalid App Attest data.".into()))?;
    let app_id = format!("{team_id}.{}", state.config.apple_bundle_id);
    let (public_key, receipt) = attestation
        .verify(&request.challenge, &app_id, &request.key_id, &root)
        .map_err(|_| {
            (
                StatusCode::UNAUTHORIZED,
                "App Attest verification failed.".into(),
            )
        })?;
    let used = trial_was_used(&state, &request.device_token).await?;
    crate::db::save_device_attestation(
        &state.db,
        user.0.id,
        &request.key_id,
        &BASE64.encode(public_key),
        &BASE64.encode(receipt),
        &request.device_token,
        used,
    )
    .await
    .map_err(|err| (StatusCode::CONFLICT, err.to_string()))?;
    state
        .account_status(user.0.id)
        .await
        .map(Json)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))
}
