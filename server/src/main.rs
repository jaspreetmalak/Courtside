mod api;
mod bridge;
mod config;
mod db;
mod number_lifecycle;
mod state;
mod twilio;
mod web;

use axum::routing::{any, get, post};
use axum::Router;
use tracing_subscriber::EnvFilter;

use crate::state::AppState;

fn install_tls_crypto_provider() {
    // Different dependencies enable Rustls' ring and aws-lc-rs backends.
    // Select one before any TLS client is constructed so the Realtime
    // WebSocket cannot panic when a Twilio media stream connects.
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    install_tls_crypto_provider();
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = config::Config::from_env()?;
    if config.dev_bypass_phone_verification {
        tracing::warn!(
            "DEV_BYPASS_PHONE_VERIFICATION is enabled; accounts can use trials without phone verification"
        );
    }
    if config.dev_bypass_usage_limits {
        tracing::warn!(
            "DEV_BYPASS_USAGE_LIMITS is enabled; calls will not consume development entitlements"
        );
    }
    let db = db::connect(&config.database_url).await?;
    tracing::info!(bind = %config.bind_addr, "Courtside server starting");

    let state = AppState::new(db, config);
    number_lifecycle::start(state.clone());
    if let Err(err) = api::numbers::sync_voice_webhooks(&state).await {
        // A temporary Twilio outage should not prevent the local API from
        // starting; number claims and calls will surface their own errors.
        tracing::warn!(?err, "failed to refresh Twilio voice webhooks");
    }

    // Everything under /api except signup/login requires a bearer token.
    let protected = Router::new()
        .route(
            "/api/calls",
            post(api::routes::start_call).get(api::routes::list_calls),
        )
        .route("/api/calls/{id}", get(api::routes::call_detail))
        .route("/api/calls/{id}/end", post(api::routes::end_call))
        .route("/api/calls/{id}/takeover", post(api::routes::takeover_call))
        .route(
            "/api/calls/{id}/handoff/decline",
            post(api::routes::decline_call_handoff),
        )
        .route(
            "/api/calls/{id}/resume",
            post(api::routes::resume_assistant),
        )
        .route("/api/ws", any(api::ws::events_handler))
        .route("/api/calls/{id}/audio", any(api::ws::call_audio_handler))
        .route("/api/calls/{id}/listen", any(api::ws::call_listen_handler))
        .route("/api/calls/{id}/voice", any(api::ws::call_voice_handler))
        .route("/api/me", get(api::auth::me))
        .route("/api/account/status", get(api::account::status))
        .merge(api::history_settings::router())
        .route(
            "/api/account/call-preferences",
            get(api::account::call_preferences).put(api::account::update_call_preferences),
        )
        .route("/api/verification/start", post(api::verification::start))
        .route(
            "/api/verification/confirm",
            post(api::verification::confirm),
        )
        .route("/api/device/challenge", get(api::device::challenge))
        .route("/api/device/attest", post(api::device::attest))
        .route("/api/subscription/sync", post(api::subscription::sync))
        .route("/api/chat", get(api::chat::history).post(api::chat::send))
        .route("/api/chat/new", post(api::chat::new_chat))
        .route("/api/chats", get(api::chat::recent))
        .route("/api/chats/{id}/select", post(api::chat::select))
        .route("/api/numbers", get(api::numbers::owned))
        .route("/api/numbers/available", get(api::numbers::search))
        .route("/api/numbers/purchase", post(api::numbers::purchase))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            api::auth::require_auth,
        ));

    let app = Router::new()
        // Twilio-facing
        .route("/twilio/voice", post(twilio::webhook::voice_webhook))
        .route("/twilio/status", post(twilio::webhook::status_webhook))
        .route("/twilio/resume", post(twilio::webhook::resume_webhook))
        .route(
            "/twilio/takeover/status",
            post(twilio::webhook::takeover_status_webhook),
        )
        .route(
            "/twilio/takeover/finished",
            post(twilio::webhook::takeover_finished_webhook),
        )
        .route(
            "/twilio/media",
            any(bridge::call_bridge::media_stream_handler),
        )
        .route(
            "/twilio/observe",
            any(bridge::call_bridge::observe_stream_handler),
        )
        // Client-facing
        .route("/api/auth/signup", post(api::auth::signup))
        .route("/api/auth/login", post(api::auth::login))
        .route(
            "/api/apple/notifications",
            post(api::subscription::notification),
        )
        .merge(protected)
        .route("/health", get(|| async { "ok" }))
        .with_state(state.clone())
        .layer(web::cors_from_env()?);

    let listener = tokio::net::TcpListener::bind(&state.config.bind_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn installs_a_process_wide_tls_crypto_provider() {
        super::install_tls_crypto_provider();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());

        // This is the builder used transitively by the Realtime WebSocket.
        // It panics when both providers are enabled but neither is selected.
        let _ = rustls::ClientConfig::builder();
    }
}
