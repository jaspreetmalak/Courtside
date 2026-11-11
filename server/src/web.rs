//! Browser clients use the same bearer-authenticated API as the native app.
use axum::http::{header, HeaderName, HeaderValue, Method};
use tower_http::cors::CorsLayer;

pub fn cors_from_env() -> anyhow::Result<CorsLayer> {
    let origins = std::env::var("WEB_ORIGINS")
        .unwrap_or_else(|_| "http://127.0.0.1:8080,http://localhost:8080".into());
    cors(&origins)
}

fn cors(origins: &str) -> anyhow::Result<CorsLayer> {
    let origins = origins
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|origin| {
            let url = reqwest::Url::parse(origin)?;
            anyhow::ensure!(
                matches!(url.scheme(), "http" | "https")
                    && url.origin().ascii_serialization() == origin,
                "WEB_ORIGINS must contain exact http(s) origins, without a path or wildcard"
            );
            Ok(HeaderValue::from_str(origin)?)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            HeaderName::from_static("ngrok-skip-browser-warning"),
        ])
        .max_age(std::time::Duration::from_secs(600)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::get, Router};

    #[tokio::test]
    async fn browser_preflight_allows_only_the_configured_origin_and_keeps_auth_required() {
        let state = crate::state::AppState::new(
            crate::db::connect("sqlite::memory:").await.unwrap(),
            crate::config::Config::for_test(),
        );
        let app = Router::new()
            .route("/api/me", get(crate::api::auth::me))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                crate::api::auth::require_auth,
            ))
            .with_state(state)
            .layer(cors("https://courtside.example").unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/api/me", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::new();
        let allowed = client
            .request(Method::OPTIONS, &url)
            .header(header::ORIGIN, "https://courtside.example")
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
            .header(
                header::ACCESS_CONTROL_REQUEST_HEADERS,
                "authorization,ngrok-skip-browser-warning",
            )
            .send()
            .await
            .unwrap();
        assert_eq!(allowed.status(), 200);
        assert_eq!(
            allowed.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
            "https://courtside.example"
        );
        assert!(allowed.headers()[header::ACCESS_CONTROL_ALLOW_HEADERS]
            .to_str()
            .unwrap()
            .contains("authorization"));
        assert!(allowed.headers()[header::ACCESS_CONTROL_ALLOW_METHODS]
            .to_str()
            .unwrap()
            .contains("DELETE"));
        assert!(!allowed
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_CREDENTIALS));
        let denied = client
            .request(Method::OPTIONS, &url)
            .header(header::ORIGIN, "https://other.example")
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
            .send()
            .await
            .unwrap();
        assert!(!denied
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
        let unauthenticated = client
            .get(&url)
            .header(header::ORIGIN, "https://courtside.example")
            .send()
            .await
            .unwrap();
        assert_eq!(unauthenticated.status(), 401);
        assert_eq!(
            unauthenticated.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
            "https://courtside.example"
        );
        server.abort();
    }

    #[test]
    fn rejects_wildcards_and_non_origin_configuration() {
        assert!(cors("*").is_err());
        assert!(cors("https://courtside.example/app").is_err());
        assert!(cors("https://courtside.example/").is_err());
        assert!(cors("https://courtside.example,http://localhost:8080").is_ok());
    }
}
