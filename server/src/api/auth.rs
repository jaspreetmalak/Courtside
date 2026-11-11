//! Signup/login with argon2 password hashes and opaque bearer tokens.

use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use axum::Json;
use shared::{AuthResponse, LoginRequest, SignupRequest, UserProfile};

use crate::db::entities::users;
use crate::state::AppState;

type ApiError = (StatusCode, String);

fn internal(err: impl std::fmt::Display) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}

pub async fn signup(
    State(state): State<AppState>,
    Json(request): Json<SignupRequest>,
) -> Result<Json<AuthResponse>, ApiError> {
    let email = request.email.trim().to_lowercase();
    if !email.contains('@') || email.len() < 5 {
        return Err((StatusCode::BAD_REQUEST, "enter a valid email".into()));
    }
    if request.password.len() < 8 {
        return Err((
            StatusCode::BAD_REQUEST,
            "password must be at least 8 characters".into(),
        ));
    }
    let display_name = request.display_name.trim();
    if display_name.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "enter your name".into()));
    }
    let custom_instructions = request.custom_instructions.trim();
    if custom_instructions.chars().count() > 800 {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "keep custom call instructions under 800 characters".into(),
        ));
    }
    let custom_instructions = (!custom_instructions.is_empty()).then_some(custom_instructions);
    if crate::db::find_user_by_email(&state.db, &email)
        .await
        .map_err(internal)?
        .is_some()
    {
        return Err((
            StatusCode::CONFLICT,
            "an account with this email already exists".into(),
        ));
    }

    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(request.password.as_bytes(), &salt)
        .map_err(internal)?
        .to_string();

    let user = crate::db::create_user_with_preferences(
        &state.db,
        &email,
        display_name,
        &hash,
        request.voice,
        request.announce_courtside,
        custom_instructions,
    )
    .await
    .map_err(internal)?;
    let token = crate::db::insert_token(&state.db, user.id)
        .await
        .map_err(internal)?;
    Ok(Json(AuthResponse {
        token,
        user: crate::db::user_to_profile(&user),
    }))
}

pub async fn login(
    State(state): State<AppState>,
    Json(request): Json<LoginRequest>,
) -> Result<Json<AuthResponse>, ApiError> {
    let invalid = || {
        (
            StatusCode::UNAUTHORIZED,
            "wrong email or password".to_string(),
        )
    };
    let user = crate::db::find_user_by_email(&state.db, request.email.trim())
        .await
        .map_err(internal)?
        .ok_or_else(invalid)?;
    let parsed = PasswordHash::new(&user.password_hash).map_err(internal)?;
    Argon2::default()
        .verify_password(request.password.as_bytes(), &parsed)
        .map_err(|_| invalid())?;
    let token = crate::db::insert_token(&state.db, user.id)
        .await
        .map_err(internal)?;
    Ok(Json(AuthResponse {
        token,
        user: crate::db::user_to_profile(&user),
    }))
}

pub async fn me(user: AuthedUser) -> Json<UserProfile> {
    Json(crate::db::user_to_profile(&user.0))
}

/// The authenticated user, inserted by [`require_auth`].
#[derive(Clone)]
pub struct AuthedUser(pub users::Model);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for AuthedUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<AuthedUser>()
            .cloned()
            .ok_or((StatusCode::UNAUTHORIZED, "not signed in".into()))
    }
}

/// Middleware for the protected `/api` routes: resolves the bearer token
/// (Authorization header, or `?token=` for WebSocket clients) to a user.
pub async fn require_auth(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let token = bearer_token(&request)
        .ok_or((StatusCode::UNAUTHORIZED, "missing bearer token".to_string()))?;
    let user = crate::db::user_for_token(&state.db, &token)
        .await
        .map_err(internal)?
        .ok_or((
            StatusCode::UNAUTHORIZED,
            "invalid or expired token".to_string(),
        ))?;
    request.extensions_mut().insert(AuthedUser(user));
    Ok(next.run(request).await)
}

fn bearer_token(request: &Request) -> Option<String> {
    if let Some(value) = request.headers().get(header::AUTHORIZATION) {
        let value = value.to_str().ok()?;
        return value.strip_prefix("Bearer ").map(str::to_string);
    }
    // WebSocket clients can't always set headers; accept ?token=.
    let query = request.uri().query()?;
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix("token="))
        .map(str::to_string)
}
