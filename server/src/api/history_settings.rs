use super::auth::AuthedUser;
use crate::{db::history_settings, state::AppState};
use axum::{extract::State, http::StatusCode, Json};
use shared::{HistorySettingsResponse, UpdateHistorySettingsRequest};
type ApiError = (StatusCode, String);

pub fn router() -> axum::Router<AppState> {
    axum::Router::new().route(
        "/api/account/history-settings",
        axum::routing::get(get).put(update),
    )
}
fn internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(%error, "chat history settings failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Past-chat settings are unavailable. Please try again.".into(),
    )
}
async fn get(
    State(state): State<AppState>,
    user: AuthedUser,
) -> Result<Json<HistorySettingsResponse>, ApiError> {
    history_settings::get(&state.db, user.0.id)
        .await
        .map(Json)
        .map_err(internal)
}
async fn update(
    State(state): State<AppState>,
    user: AuthedUser,
    Json(request): Json<UpdateHistorySettingsRequest>,
) -> Result<Json<HistorySettingsResponse>, ApiError> {
    history_settings::set_enabled(&state.db, user.0.id, request.enabled)
        .await
        .map(Json)
        .map_err(internal)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn history_preference_requires_auth_and_changes_only_its_owner() {
        let db = crate::db::connect("sqlite::memory:").await.unwrap();
        let owner = crate::db::create_user(&db, "history-setting@example.test", "Owner", "unused")
            .await
            .unwrap();
        let other =
            crate::db::create_user(&db, "history-setting-other@example.test", "Other", "unused")
                .await
                .unwrap();
        let token = crate::db::insert_token(&db, owner.id).await.unwrap();
        let other_token = crate::db::insert_token(&db, other.id).await.unwrap();
        let state = AppState::new(db, crate::config::Config::for_test());
        let app = router()
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
        let client = reqwest::Client::new();
        let url = format!("http://{address}/api/account/history-settings");
        assert_eq!(
            client.get(&url).send().await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        let changed = client
            .put(&url)
            .bearer_auth(&token)
            .json(&UpdateHistorySettingsRequest { enabled: false })
            .send()
            .await
            .unwrap();
        assert_eq!(changed.status(), StatusCode::OK);
        assert!(
            !changed
                .json::<HistorySettingsResponse>()
                .await
                .unwrap()
                .enabled
        );
        assert!(
            client
                .get(&url)
                .bearer_auth(other_token)
                .send()
                .await
                .unwrap()
                .json::<HistorySettingsResponse>()
                .await
                .unwrap()
                .enabled
        );
        assert!(!history_settings::enabled(&state.db, owner.id)
            .await
            .unwrap());
        let removed = client
            .get(format!("http://{address}/api/account/memory"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        assert_eq!(removed.status(), StatusCode::NOT_FOUND);
        server.abort();
    }
}
