use anyhow::{bail, Context, Result};

#[derive(Debug, Clone)]
pub struct Config {
    /// Local mock model socket for offline bridge integration tests only.
    #[cfg(test)]
    pub test_realtime_url: Option<String>,
    #[cfg(test)]
    pub test_responses_url: Option<String>,
    #[cfg(test)]
    pub test_twilio_rest_url: Option<String>,
    pub openai_api_key: String,
    pub twilio_account_sid: String,
    /// Restricted Twilio API key used for outbound REST requests.
    pub twilio_api_key_sid: String,
    /// Secret paired with `twilio_api_key_sid`.
    pub twilio_api_secret: String,
    /// Publicly reachable host for Twilio webhooks/streams, without scheme
    /// (e.g. "abc123.ngrok.app").
    pub public_host: String,
    pub database_url: String,
    pub bind_addr: String,
    /// Realtime model for live calls.
    pub realtime_model: String,
    /// Low-cost text model for the in-app assistant chat.
    pub chat_model: String,
    /// Reasoning model for call planning and call-control decisions.
    pub decision_model: String,
    /// Text model for post-call summaries.
    pub summary_model: String,
    /// Twilio Verify Service SID (VA...). Phone verification is unavailable
    /// until this is configured.
    pub twilio_verify_service_sid: Option<String>,
    /// Optional Verify-capable API key. Some restricted keys that can manage
    /// phone-number inventory do not have access to Twilio Verify.
    pub twilio_verify_api_key_sid: Option<String>,
    pub twilio_verify_api_secret: Option<String>,
    /// Account Auth Token fallback for Verify and Lookup.
    pub twilio_auth_token: Option<String>,
    /// Local-development escape hatch. Never enable this in production.
    pub dev_bypass_phone_verification: bool,
    /// Ignore cumulative trial/subscription usage while testing locally.
    /// Never enable this in production.
    pub dev_bypass_usage_limits: bool,
    /// Server-side pepper used before storing a uniqueness hash of verified phones.
    pub phone_hash_secret: String,
    pub app_store_product_id: String,
    pub apple_bundle_id: String,
    pub apple_app_id: Option<i64>,
    /// DER-encoded Apple Root CA certificate used for StoreKit JWS verification.
    pub apple_root_ca_path: Option<String>,
    pub apple_environment: String,
    pub require_app_attest: bool,
    pub apple_team_id: Option<String>,
    pub app_attest_root_ca_path: Option<String>,
    pub devicecheck_key_id: Option<String>,
    pub devicecheck_private_key_path: Option<String>,
}

impl Config {
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self {
            test_realtime_url: None,
            test_responses_url: None,
            test_twilio_rest_url: None,
            openai_api_key: "test".into(),
            twilio_account_sid: "test".into(),
            twilio_api_key_sid: "test".into(),
            twilio_api_secret: "test".into(),
            public_host: "example.com".into(),
            database_url: "sqlite::memory:".into(),
            bind_addr: "127.0.0.1:0".into(),
            realtime_model: "test".into(),
            chat_model: "test".into(),
            decision_model: "test".into(),
            summary_model: "test".into(),
            twilio_verify_service_sid: None,
            twilio_verify_api_key_sid: None,
            twilio_verify_api_secret: None,
            twilio_auth_token: None,
            dev_bypass_phone_verification: false,
            dev_bypass_usage_limits: false,
            phone_hash_secret: "test".into(),
            app_store_product_id: "test".into(),
            apple_bundle_id: "test".into(),
            apple_app_id: None,
            apple_root_ca_path: None,
            apple_environment: "sandbox".into(),
            require_app_attest: false,
            apple_team_id: None,
            app_attest_root_ca_path: None,
            devicecheck_key_id: None,
            devicecheck_private_key_path: None,
        }
    }

    pub fn from_env() -> Result<Self> {
        let var = |key: &str| std::env::var(key).with_context(|| format!("missing env var {key}"));
        let twilio_api_secret = std::env::var("TWILIO_API_SECRET")
            .or_else(|_| std::env::var("TWILIO_API_CLIENT_SECRET"))
            .context(
                "missing env var TWILIO_API_SECRET (TWILIO_API_CLIENT_SECRET is also supported)",
            )?;
        Ok(Self {
            #[cfg(test)]
            test_realtime_url: None,
            #[cfg(test)]
            test_responses_url: None,
            #[cfg(test)]
            test_twilio_rest_url: None,
            openai_api_key: var("OPENAI_API_KEY")?,
            twilio_account_sid: var("TWILIO_ACCOUNT_SID")?,
            twilio_api_key_sid: var("TWILIO_API_KEY_SID")?,
            twilio_api_secret,
            public_host: normalize_public_host(&var("PUBLIC_HOST")?)?,
            database_url: std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| "sqlite://courtside.db?mode=rwc".into()),
            bind_addr: std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".into()),
            realtime_model: std::env::var("REALTIME_MODEL").unwrap_or_else(|_| "gpt-live-1".into()),
            chat_model: std::env::var("CHAT_MODEL").unwrap_or_else(|_| "gpt-6-luna".into()),
            decision_model: std::env::var("DECISION_MODEL")
                .unwrap_or_else(|_| "gpt-5.6-terra".into()),
            summary_model: std::env::var("SUMMARY_MODEL").unwrap_or_else(|_| "gpt-6-luna".into()),
            twilio_verify_service_sid: std::env::var("TWILIO_VERIFY_SERVICE_SID").ok(),
            twilio_verify_api_key_sid: std::env::var("TWILIO_VERIFY_API_KEY_SID").ok(),
            twilio_verify_api_secret: std::env::var("TWILIO_VERIFY_API_SECRET").ok(),
            twilio_auth_token: std::env::var("TWILIO_AUTH_TOKEN").ok(),
            dev_bypass_phone_verification: std::env::var("DEV_BYPASS_PHONE_VERIFICATION")
                .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE")),
            dev_bypass_usage_limits: std::env::var("DEV_BYPASS_USAGE_LIMITS")
                .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE")),
            phone_hash_secret: std::env::var("PHONE_HASH_SECRET")
                .unwrap_or_else(|_| "local-development-change-me".into()),
            app_store_product_id: std::env::var("APP_STORE_PRODUCT_ID")
                .unwrap_or_else(|_| "com.malakindustries.courtside.pro.monthly".into()),
            apple_bundle_id: std::env::var("APPLE_BUNDLE_ID")
                .unwrap_or_else(|_| "com.malakindustries.courtside".into()),
            apple_app_id: std::env::var("APPLE_APP_ID")
                .ok()
                .and_then(|v| v.parse().ok()),
            apple_root_ca_path: std::env::var("APPLE_ROOT_CA_PATH").ok(),
            apple_environment: std::env::var("APPLE_ENVIRONMENT")
                .unwrap_or_else(|_| "sandbox".into()),
            require_app_attest: std::env::var("REQUIRE_APP_ATTEST")
                .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE")),
            apple_team_id: std::env::var("APPLE_TEAM_ID").ok(),
            app_attest_root_ca_path: std::env::var("APP_ATTEST_ROOT_CA_PATH").ok(),
            devicecheck_key_id: std::env::var("DEVICECHECK_KEY_ID").ok(),
            devicecheck_private_key_path: std::env::var("DEVICECHECK_PRIVATE_KEY_PATH").ok(),
        })
    }
}

fn normalize_public_host(value: &str) -> Result<String> {
    let host = value
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string();
    if host.is_empty() || host.contains('/') || host.contains("example.ngrok") {
        bail!(
            "PUBLIC_HOST is a placeholder or invalid; start development with ./scripts/run-local.sh"
        );
    }
    Ok(host)
}

#[cfg(test)]
mod tests {
    use super::normalize_public_host;

    #[test]
    fn rejects_placeholder_and_normalizes_real_tunnel() {
        assert!(normalize_public_host("example.ngrok.app").is_err());
        assert_eq!(
            normalize_public_host("https://active-tunnel.ngrok-free.dev/").unwrap(),
            "active-tunnel.ngrok-free.dev"
        );
    }
}
