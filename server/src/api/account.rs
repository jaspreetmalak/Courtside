use axum::{extract::State, http::StatusCode, Json};
use shared::{AccountStatusResponse, CallPreferences, UpdateCallPreferencesRequest};

use super::auth::AuthedUser;
use crate::state::AppState;

pub async fn status(
    State(state): State<AppState>,
    user: AuthedUser,
) -> Result<Json<AccountStatusResponse>, (StatusCode, String)> {
    state
        .account_status(user.0.id)
        .await
        .map(Json)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))
}

pub async fn call_preferences(
    user: AuthedUser,
) -> Result<Json<CallPreferences>, (StatusCode, String)> {
    Ok(Json(crate::db::call_preferences(&user.0)))
}

pub async fn update_call_preferences(
    State(state): State<AppState>,
    user: AuthedUser,
    Json(request): Json<UpdateCallPreferencesRequest>,
) -> Result<Json<CallPreferences>, (StatusCode, String)> {
    let custom_instructions = request.custom_instructions.trim();
    if custom_instructions.chars().count() > 800 {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "Keep custom call instructions under 800 characters.".into(),
        ));
    }
    let custom_instructions = (!custom_instructions.is_empty()).then_some(custom_instructions);
    let takeover_phone = request.takeover_phone_number.trim();
    let takeover_phone = if takeover_phone.is_empty() {
        None
    } else {
        Some(
            crate::api::routes::normalize_nanp_destination(takeover_phone).ok_or((
                StatusCode::BAD_REQUEST,
                "Enter a valid US/Canada takeover number.".into(),
            ))?,
        )
    };
    crate::db::update_call_preferences(
        &state.db,
        user.0,
        request.voice,
        request.announce_courtside,
        custom_instructions,
        takeover_phone.as_deref(),
    )
    .await
    .map(Json)
    .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))
}
