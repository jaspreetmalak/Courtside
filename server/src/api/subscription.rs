use std::fs;

use app_store_server_library::{
    models::{
        app_store_environment::Environment, auto_renew_status::AutoRenewStatus, status::Status,
    },
    signed_data_verifier::SignedDataVerifier,
};
use axum::{extract::State, http::StatusCode, Json};
use chrono::Utc;
use serde::Deserialize;
use shared::{SubscriptionSyncRequest, SubscriptionSyncResponse};

use super::auth::AuthedUser;
use crate::state::AppState;

type ApiError = (StatusCode, String);

#[derive(Deserialize)]
pub struct NotificationRequest {
    #[serde(rename = "signedPayload")]
    signed_payload: String,
}

fn verifier(state: &AppState) -> Result<SignedDataVerifier, ApiError> {
    let path = state.config.apple_root_ca_path.as_deref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "App Store verification is not configured. Set APPLE_ROOT_CA_PATH.".into(),
    ))?;
    let cert = fs::read(path).map_err(|err| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("Unable to read Apple root certificate: {err}"),
        )
    })?;
    let environment = match state.config.apple_environment.as_str() {
        "production" => Environment::Production,
        _ => Environment::Sandbox,
    };
    SignedDataVerifier::new(
        vec![cert],
        environment,
        state.config.apple_bundle_id.clone(),
        state.config.apple_app_id,
        true,
    )
    .map_err(|err| (StatusCode::SERVICE_UNAVAILABLE, err.to_string()))
}

async fn apply_transaction(
    state: &AppState,
    user_id: uuid::Uuid,
    signed_transaction: &str,
    status_hint: Option<&Status>,
    auto_renew: bool,
) -> Result<(), ApiError> {
    let transaction = verifier(state)?
        .verify_and_decode_signed_transaction(signed_transaction)
        .map_err(|_| {
            (
                StatusCode::UNAUTHORIZED,
                "Invalid App Store transaction.".into(),
            )
        })?;
    if transaction.product_id.as_deref() != Some(&state.config.app_store_product_id) {
        return Err((
            StatusCode::BAD_REQUEST,
            "Unexpected App Store product.".into(),
        ));
    }
    if transaction.app_account_token != Some(user_id) {
        return Err((
            StatusCode::UNAUTHORIZED,
            "This App Store transaction belongs to a different Courtside account.".into(),
        ));
    }
    let original_id = transaction.original_transaction_id.ok_or((
        StatusCode::BAD_REQUEST,
        "Transaction is missing its original identifier.".into(),
    ))?;
    let period_start = transaction.purchase_date.ok_or((
        StatusCode::BAD_REQUEST,
        "Transaction is missing its purchase date.".into(),
    ))?;
    let period_end = transaction.expires_date.ok_or((
        StatusCode::BAD_REQUEST,
        "Transaction is missing its expiration date.".into(),
    ))?;
    let revoked = transaction.revocation_date.is_some();
    let status = if revoked || period_end <= Utc::now() {
        "expired"
    } else if matches!(status_hint, Some(Status::BillingGracePeriod)) {
        "grace_period"
    } else {
        "active"
    };
    let environment = transaction
        .environment
        .map(|value| format!("{value:?}").to_lowercase())
        .unwrap_or_else(|| state.config.apple_environment.clone());
    crate::db::upsert_subscription(
        &state.db,
        user_id,
        &state.config.app_store_product_id,
        &original_id,
        status,
        period_start,
        period_end,
        auto_renew,
        &environment,
    )
    .await
    .map_err(|err| (StatusCode::CONFLICT, err.to_string()))?;
    Ok(())
}

pub async fn sync(
    State(state): State<AppState>,
    user: AuthedUser,
    Json(request): Json<SubscriptionSyncRequest>,
) -> Result<Json<SubscriptionSyncResponse>, ApiError> {
    let account = state
        .account_status(user.0.id)
        .await
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
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
    apply_transaction(
        &state,
        user.0.id,
        &request.signed_transaction_info,
        None,
        true,
    )
    .await?;
    let account = state
        .account_status(user.0.id)
        .await
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    Ok(Json(SubscriptionSyncResponse { account }))
}

pub async fn notification(
    State(state): State<AppState>,
    Json(request): Json<NotificationRequest>,
) -> Result<StatusCode, ApiError> {
    let verifier = verifier(&state)?;
    let notification = verifier
        .verify_and_decode_notification(&request.signed_payload)
        .map_err(|_| {
            (
                StatusCode::UNAUTHORIZED,
                "Invalid App Store notification.".into(),
            )
        })?;
    let Some(data) = notification.data else {
        return Ok(StatusCode::NO_CONTENT);
    };
    let Some(signed_transaction) = data.signed_transaction_info.as_deref() else {
        return Ok(StatusCode::NO_CONTENT);
    };
    let transaction = verifier
        .verify_and_decode_signed_transaction(signed_transaction)
        .map_err(|_| {
            (
                StatusCode::UNAUTHORIZED,
                "Invalid nested transaction.".into(),
            )
        })?;
    let Some(user_id) = transaction.app_account_token else {
        return Ok(StatusCode::NO_CONTENT);
    };
    let auto_renew = match data.signed_renewal_info.as_deref() {
        Some(signed) => verifier
            .verify_and_decode_renewal_info(signed)
            .ok()
            .and_then(|renewal| renewal.auto_renew_status)
            .is_some_and(|status| status == AutoRenewStatus::On),
        None => false,
    };
    apply_transaction(
        &state,
        user_id,
        signed_transaction,
        data.status.as_ref(),
        auto_renew,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
