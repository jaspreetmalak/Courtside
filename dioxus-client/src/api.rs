//! Native HTTP bridge to the existing Courtside Axum server.

use serde::de::DeserializeOwned;
use shared::{
    AccountStatusResponse, AuthResponse, AvailableNumbersResponse, CallControlResponse,
    CallDetailResponse, CallPreferences, ChatHistoryResponse, HistorySettingsResponse,
    ListCallsResponse, LoginRequest, OwnedNumber, PhoneVerificationConfirmRequest,
    PhoneVerificationStartRequest, PhoneVerificationStartResponse, PurchaseNumberRequest,
    RecentChatsResponse, SendChatRequest, SendChatResponse, ServerEvent, SignupRequest,
    UpdateCallPreferencesRequest, UpdateHistorySettingsRequest,
};
use std::sync::Mutex;
use uuid::Uuid;

pub fn base_url() -> &'static str {
    option_env!("COURTSIDE_SERVER_URL").unwrap_or("http://127.0.0.1:3000")
}

static TOKEN: Mutex<Option<String>> = Mutex::new(None);

pub fn set_token(token: Option<String>) {
    *TOKEN.lock().expect("token mutex poisoned") = token;
}

fn token() -> Option<String> {
    TOKEN.lock().expect("token mutex poisoned").clone()
}

/// Open the authenticated live event stream on a background thread. The
/// returned channel carries call status, transcript, summary, and proactive
/// chat updates without making the user refresh or ask what happened.
#[cfg(not(target_arch = "wasm32"))]
pub fn subscribe_events() -> ApiResult<async_channel::Receiver<ServerEvent>> {
    let bearer = token().ok_or_else(|| "Not signed in.".to_string())?;
    let http_base = base_url().trim_end_matches('/');
    let ws_base = if let Some(host) = http_base.strip_prefix("https://") {
        format!("wss://{host}")
    } else if let Some(host) = http_base.strip_prefix("http://") {
        format!("ws://{host}")
    } else {
        return Err("The Courtside server URL must start with http:// or https://.".into());
    };
    let url = format!("{ws_base}/api/ws?token={bearer}");
    let (tx, rx) = async_channel::unbounded();

    std::thread::spawn(move || {
        while !tx.is_closed() {
            match tungstenite::connect(&url) {
                Ok((mut socket, _)) => loop {
                    match socket.read() {
                        Ok(message) if message.is_text() => {
                            let Ok(text) = message.to_text() else {
                                continue;
                            };
                            let Ok(event) = serde_json::from_str::<ServerEvent>(text) else {
                                continue;
                            };
                            if tx.send_blocking(event).is_err() {
                                return;
                            }
                        }
                        Ok(message) if message.is_close() => break,
                        Ok(_) => {}
                        Err(_) => break,
                    }
                },
                Err(_) => {}
            }

            if tx.is_closed() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    });

    Ok(rx)
}

pub type ApiResult<T> = Result<T, String>;

#[cfg(not(target_arch = "wasm32"))]
async fn request<T: DeserializeOwned + Send + 'static>(
    method: &'static str,
    path: String,
    body: Option<serde_json::Value>,
) -> ApiResult<T> {
    let (tx, rx) = async_channel::bounded::<ApiResult<T>>(1);
    let bearer = token();

    std::thread::spawn(move || {
        let result = request_sync(method, &path, body, bearer.as_deref());
        let _ = tx.send_blocking(result);
    });

    rx.recv()
        .await
        .map_err(|_| "The request worker stopped unexpectedly.".to_string())?
}

#[cfg(not(target_arch = "wasm32"))]
fn request_sync<T: DeserializeOwned>(
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
    bearer: Option<&str>,
) -> ApiResult<T> {
    let url = format!("{}{}", base_url(), path);
    let timeout = if path == "/api/ai/benchmark" {
        std::time::Duration::from_secs(600)
    } else {
        std::time::Duration::from_secs(30)
    };
    let agent = ureq::AgentBuilder::new().timeout(timeout).build();
    let mut request = match method {
        "POST" => agent.post(&url),
        "PUT" => agent.put(&url),
        "DELETE" => agent.delete(&url),
        _ => agent.get(&url),
    };
    if let Some(bearer) = bearer {
        request = request.set("Authorization", &format!("Bearer {bearer}"));
    }

    let response = match body {
        Some(body) => request.send_json(body),
        None => request.call(),
    }
    .map_err(read_http_error)?;

    response.into_json::<T>().map_err(|error| error.to_string())
}

/// Recover live state after a missed WebSocket event or app suspension. The
/// screen dropping its receiver stops this worker without leaking a poller.
#[cfg(not(target_arch = "wasm32"))]
pub fn watch_call(id: Uuid) -> ApiResult<async_channel::Receiver<CallDetailResponse>> {
    let bearer = token().ok_or_else(|| "Not signed in.".to_string())?;
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let path = format!("/api/calls/{id}");
        while !tx.is_closed() {
            if let Ok(detail) =
                request_sync::<CallDetailResponse>("GET", &path, None, Some(&bearer))
            {
                let ended = matches!(
                    detail.call.status,
                    shared::CallStatus::Completed | shared::CallStatus::Failed
                );
                if tx.send_blocking(detail).is_err() || ended {
                    break;
                }
            }
            for _ in 0..20 {
                if tx.is_closed() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    });
    Ok(rx)
}

#[cfg(not(target_arch = "wasm32"))]
fn read_http_error(error: ureq::Error) -> String {
    match error {
        ureq::Error::Status(code, response) => {
            let body = response.into_string().unwrap_or_default();
            if body.contains("credit_balance_exhausted") || body.contains("insufficient_quota") {
                "OpenAI API credits are exhausted. Add credits in the OpenAI dashboard.".into()
            } else if !body.is_empty() && body.len() < 600 {
                body
            } else {
                format!("Request failed ({code}).")
            }
        }
        other => format!("Could not reach Courtside: {other}"),
    }
}

#[cfg(target_arch = "wasm32")]
async fn request<T: DeserializeOwned + 'static>(
    method: &'static str,
    path: String,
    body: Option<serde_json::Value>,
) -> ApiResult<T> {
    #[derive(serde::Deserialize)]
    struct Response {
        ok: bool,
        status: u16,
        body: String,
    }
    let config = serde_json::json!({
        "url": format!("{}{}", base_url(), path),
        "method": method, "body": body, "token": token(),
    });
    let script = include_str!("browser_request.js").replace("__CONFIG__", &config.to_string());
    let mut eval = dioxus::document::eval(&script);
    let response = eval
        .recv::<Response>()
        .await
        .map_err(|_| "The browser request stopped. Please try again.".to_string())?;
    if !response.ok {
        return Err(if response.status == 0 {
            "Could not reach Courtside. Check your connection and try again.".into()
        } else if response.body.len() < 600 && !response.body.is_empty() {
            response.body
        } else {
            format!("Request failed ({}).", response.status)
        });
    }
    serde_json::from_str(&response.body)
        .map_err(|_| "Courtside returned an unexpected response.".into())
}

#[cfg(target_arch = "wasm32")]
pub fn subscribe_events() -> ApiResult<async_channel::Receiver<ServerEvent>> {
    let bearer = token().ok_or_else(|| "Not signed in.".to_string())?;
    let url = format!("{}/api/ws?token={bearer}", websocket_base()?);
    let config = serde_json::json!({"url":url});
    let script = include_str!("browser_events.js").replace("__CONFIG__", &config.to_string());
    let (tx, rx) = async_channel::unbounded();
    dioxus::prelude::spawn(async move {
        let mut eval = dioxus::document::eval(&script);
        while let Ok(event) = eval.recv::<ServerEvent>().await {
            if tx.send(event).await.is_err() {
                break;
            }
        }
    });
    Ok(rx)
}

pub fn stop_events() {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = dioxus::document::eval("window.__courtsideEvents?.stop();");
    }
}

#[cfg(target_arch = "wasm32")]
pub fn watch_call(id: Uuid) -> ApiResult<async_channel::Receiver<CallDetailResponse>> {
    token().ok_or_else(|| "Not signed in.".to_string())?;
    let (tx, rx) = async_channel::bounded(1);
    dioxus::prelude::spawn(async move {
        while !tx.is_closed() {
            if let Ok(detail) = call_detail(id).await {
                let ended = matches!(
                    detail.call.status,
                    shared::CallStatus::Completed | shared::CallStatus::Failed
                );
                if tx.send(detail).await.is_err() || ended {
                    break;
                }
            }
            let _ =
                dioxus::document::eval("await new Promise(resolve => setTimeout(resolve, 2000));")
                    .await;
        }
    });
    Ok(rx)
}

#[cfg(target_arch = "wasm32")]
fn websocket_base() -> ApiResult<String> {
    let base = base_url().trim_end_matches('/');
    if let Some(host) = base.strip_prefix("https://") {
        Ok(format!("wss://{host}"))
    } else if let Some(host) = base.strip_prefix("http://") {
        Ok(format!("ws://{host}"))
    } else {
        Err("The Courtside server URL must start with http:// or https://.".into())
    }
}

#[cfg(target_arch = "wasm32")]
pub fn browser_voice_ws_url(id: Uuid) -> ApiResult<String> {
    let bearer = token().ok_or_else(|| "Not signed in.".to_string())?;
    Ok(format!(
        "{}/api/calls/{id}/voice?token={bearer}",
        websocket_base()?
    ))
}

pub async fn signup(
    email: String,
    password: String,
    display_name: String,
    voice: shared::CallVoice,
    announce_courtside: bool,
    custom_instructions: String,
) -> ApiResult<AuthResponse> {
    let body = serde_json::to_value(SignupRequest {
        email,
        password,
        display_name,
        voice,
        announce_courtside,
        custom_instructions,
    })
    .map_err(|error| error.to_string())?;
    request("POST", "/api/auth/signup".into(), Some(body)).await
}

pub async fn login(email: String, password: String) -> ApiResult<AuthResponse> {
    let body = serde_json::to_value(LoginRequest { email, password })
        .map_err(|error| error.to_string())?;
    request("POST", "/api/auth/login".into(), Some(body)).await
}

pub async fn account_status() -> ApiResult<AccountStatusResponse> {
    request("GET", "/api/account/status".into(), None).await
}

pub async fn history_settings() -> ApiResult<HistorySettingsResponse> {
    request("GET", "/api/account/history-settings".into(), None).await
}

pub async fn set_history_enabled(enabled: bool) -> ApiResult<HistorySettingsResponse> {
    let body = serde_json::to_value(UpdateHistorySettingsRequest { enabled })
        .map_err(|e| e.to_string())?;
    request("PUT", "/api/account/history-settings".into(), Some(body)).await
}

pub async fn start_phone_verification(
    phone_number: String,
) -> ApiResult<PhoneVerificationStartResponse> {
    let body = serde_json::to_value(PhoneVerificationStartRequest { phone_number })
        .map_err(|error| error.to_string())?;
    request("POST", "/api/verification/start".into(), Some(body)).await
}

pub async fn confirm_phone_verification(
    phone_number: String,
    code: String,
) -> ApiResult<AccountStatusResponse> {
    let body = serde_json::to_value(PhoneVerificationConfirmRequest { phone_number, code })
        .map_err(|error| error.to_string())?;
    request("POST", "/api/verification/confirm".into(), Some(body)).await
}

pub async fn call_preferences() -> ApiResult<CallPreferences> {
    request("GET", "/api/account/call-preferences".into(), None).await
}

pub async fn update_call_preferences(
    voice: shared::CallVoice,
    announce_courtside: bool,
    custom_instructions: String,
    takeover_phone_number: String,
) -> ApiResult<CallPreferences> {
    let body = serde_json::to_value(UpdateCallPreferencesRequest {
        voice,
        announce_courtside,
        custom_instructions,
        takeover_phone_number,
    })
    .map_err(|error| error.to_string())?;
    request("PUT", "/api/account/call-preferences".into(), Some(body)).await
}

pub async fn list_calls() -> ApiResult<ListCallsResponse> {
    request("GET", "/api/calls".into(), None).await
}

pub async fn call_detail(id: Uuid) -> ApiResult<CallDetailResponse> {
    request("GET", format!("/api/calls/{id}"), None).await
}

pub async fn end_call(id: Uuid) -> ApiResult<CallControlResponse> {
    request("POST", format!("/api/calls/{id}/end"), None).await
}

pub async fn resume_assistant(id: Uuid) -> ApiResult<CallControlResponse> {
    request("POST", format!("/api/calls/{id}/resume"), None).await
}

pub async fn decline_call_handoff(id: Uuid, request_id: Uuid) -> ApiResult<CallControlResponse> {
    request(
        "POST",
        format!("/api/calls/{id}/handoff/decline"),
        Some(serde_json::json!({"request_id": request_id})),
    )
    .await
}

#[cfg(target_os = "ios")]
pub fn call_voice_connection(id: Uuid) -> ApiResult<(String, String)> {
    let bearer = token().ok_or_else(|| "Not signed in.".to_string())?;
    let host = base_url()
        .trim_end_matches('/')
        .strip_prefix("https://")
        .ok_or_else(|| "A secure Courtside server connection is required.".to_string())?;
    // Keep bearer credentials out of URLs, logs, and recipient metadata.
    Ok((format!("wss://{host}/api/calls/{id}/voice"), bearer))
}

#[cfg(target_os = "ios")]
pub fn call_listen_connection(id: Uuid) -> ApiResult<(String, String)> {
    let bearer = token().ok_or_else(|| "Not signed in.".to_string())?;
    let host = base_url()
        .trim_end_matches('/')
        .strip_prefix("https://")
        .ok_or_else(|| "A secure Courtside server connection is required.".to_string())?;
    Ok((format!("wss://{host}/api/calls/{id}/listen"), bearer))
}

#[cfg(not(target_os = "ios"))]
pub fn call_audio_ws_url(id: Uuid) -> ApiResult<String> {
    let bearer = token().ok_or_else(|| "Not signed in.".to_string())?;
    let http_base = base_url().trim_end_matches('/');
    let ws_base = if let Some(host) = http_base.strip_prefix("https://") {
        format!("wss://{host}")
    } else if let Some(host) = http_base.strip_prefix("http://") {
        format!("ws://{host}")
    } else {
        return Err("The Courtside server URL must start with http:// or https://.".into());
    };
    Ok(format!("{ws_base}/api/calls/{id}/listen?token={bearer}"))
}

pub async fn chat_history() -> ApiResult<ChatHistoryResponse> {
    request("GET", "/api/chat".into(), None).await
}

pub async fn send_chat(message: String, request_id: Uuid) -> ApiResult<SendChatResponse> {
    let body = serde_json::to_value(SendChatRequest {
        message,
        request_id: Some(request_id),
        call_answer: None,
    })
    .map_err(|error| error.to_string())?;
    request("POST", "/api/chat".into(), Some(body)).await
}

pub async fn send_call_answer(
    message: String,
    target: shared::CallAnswerTarget,
) -> ApiResult<SendChatResponse> {
    let body = serde_json::to_value(SendChatRequest {
        message,
        request_id: None,
        call_answer: Some(target),
    })
    .map_err(|error| error.to_string())?;
    request("POST", "/api/chat".into(), Some(body)).await
}

pub async fn new_chat() -> ApiResult<ChatHistoryResponse> {
    request("POST", "/api/chat/new".into(), None).await
}

pub async fn recent_chats() -> ApiResult<RecentChatsResponse> {
    request("GET", "/api/chats".into(), None).await
}

pub async fn select_chat(id: Uuid) -> ApiResult<ChatHistoryResponse> {
    request("POST", format!("/api/chats/{id}/select"), None).await
}

pub async fn search_numbers(area_code: String) -> ApiResult<AvailableNumbersResponse> {
    let query = if area_code.is_empty() {
        String::new()
    } else {
        format!("?area_code={area_code}")
    };
    request("GET", format!("/api/numbers/available{query}"), None).await
}

pub async fn purchase_number(phone_number: String) -> ApiResult<OwnedNumber> {
    let body = serde_json::to_value(PurchaseNumberRequest { phone_number })
        .map_err(|error| error.to_string())?;
    request("POST", "/api/numbers/purchase".into(), Some(body)).await
}
