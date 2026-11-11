//! Persisted authentication state in the application's iOS sandbox.

use serde::{Deserialize, Serialize};
use shared::UserProfile;
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub token: String,
    pub user: UserProfile,
}

#[cfg(not(target_arch = "wasm32"))]
fn path() -> PathBuf {
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    let documents = home.join("Documents");
    let directory = if documents.is_dir() { documents } else { home };
    directory.join("courtside_session.json")
}

#[cfg(not(target_arch = "wasm32"))]
pub fn load() -> Option<Session> {
    let raw = std::fs::read_to_string(path()).ok()?;
    serde_json::from_str(&raw).ok()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn save(session: &Session) {
    if let Ok(json) = serde_json::to_string(session) {
        let _ = std::fs::write(path(), json);
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn clear() {
    let _ = std::fs::remove_file(path());
}

#[cfg(target_arch = "wasm32")]
pub fn load() -> Option<Session> {
    let storage = web_sys::window()?.session_storage().ok()??;
    serde_json::from_str(&storage.get_item("courtside.session").ok()??).ok()
}

#[cfg(target_arch = "wasm32")]
pub fn save(session: &Session) {
    if let (Some(storage), Ok(json)) = (
        web_sys::window().and_then(|window| window.session_storage().ok().flatten()),
        serde_json::to_string(session),
    ) {
        let _ = storage.set_item("courtside.session", &json);
    }
}

#[cfg(target_arch = "wasm32")]
pub fn clear() {
    if let Some(storage) =
        web_sys::window().and_then(|window| window.session_storage().ok().flatten())
    {
        let _ = storage.remove_item("courtside.session");
    }
}
