//! Platform audio participation lives outside the chat component lifecycle.
use serde::Deserialize;
use std::sync::OnceLock;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Ringing,
    Declined,
    Connecting,
    Connected,
    Ended,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Event {
    pub call_id: Uuid,
    #[serde(rename = "type")]
    pub phase: Phase,
    pub message: String,
}

static EVENTS: OnceLock<(async_channel::Sender<Event>, async_channel::Receiver<Event>)> =
    OnceLock::new();

pub fn subscribe() -> async_channel::Receiver<Event> {
    EVENTS.get_or_init(async_channel::unbounded).1.clone()
}

#[cfg(target_os = "ios")]
extern "C" {
    fn courtside_voice_invite(
        url: *const std::ffi::c_char,
        token: *const std::ffi::c_char,
        call_id: *const std::ffi::c_char,
        peer: *const std::ffi::c_char,
        seconds: f64,
        callback: extern "C" fn(*const std::ffi::c_char),
    );
    fn courtside_voice_start(
        url: *const std::ffi::c_char,
        token: *const std::ffi::c_char,
        call_id: *const std::ffi::c_char,
        peer: *const std::ffi::c_char,
        callback: extern "C" fn(*const std::ffi::c_char),
    );
    fn courtside_voice_end(call_id: *const std::ffi::c_char);
    fn courtside_voice_cancel_invitation(call_id: *const std::ffi::c_char);
}

/// Incoming invitations use CallKit on iPhone. Browsers wait for a user tap
/// on Join before requesting microphone access and starting voice audio.
pub fn invite(call_id: Uuid, peer: &str, seconds: f64) -> Result<(), String> {
    #[cfg(target_os = "ios")]
    {
        use std::ffi::CString;
        let (url, token) = crate::api::call_voice_connection(call_id)?;
        let strings = [url, token, call_id.to_string(), peer.to_string()]
            .into_iter()
            .map(CString::new)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "Unable to prepare the incoming call.".to_string())?;
        unsafe {
            courtside_voice_invite(
                strings[0].as_ptr(),
                strings[1].as_ptr(),
                strings[2].as_ptr(),
                strings[3].as_ptr(),
                seconds,
                receive_event,
            );
        }
        Ok(())
    }
    #[cfg(not(target_os = "ios"))]
    {
        let _ = (call_id, peer, seconds);
        Ok(())
    }
}

#[cfg(target_os = "ios")]
extern "C" fn receive_event(json: *const std::ffi::c_char) {
    if json.is_null() {
        return;
    }
    let Ok(json) = (unsafe { std::ffi::CStr::from_ptr(json) }).to_str() else {
        return;
    };
    if let Ok(event) = serde_json::from_str::<Event>(json) {
        let _ = EVENTS
            .get_or_init(async_channel::unbounded)
            .0
            .try_send(event);
    }
}

pub fn start(call_id: Uuid, peer: &str) -> Result<(), String> {
    #[cfg(target_os = "ios")]
    {
        use std::ffi::CString;
        let (url, token) = crate::api::call_voice_connection(call_id)?;
        let strings = [url, token, call_id.to_string(), peer.to_string()]
            .into_iter()
            .map(CString::new)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "Unable to prepare call audio.".to_string())?;
        unsafe {
            courtside_voice_start(
                strings[0].as_ptr(),
                strings[1].as_ptr(),
                strings[2].as_ptr(),
                strings[3].as_ptr(),
                receive_event,
            );
        }
        Ok(())
    }
    #[cfg(target_arch = "wasm32")]
    {
        let url = crate::api::browser_voice_ws_url(call_id)?;
        let config = serde_json::json!({"url":url, "call_id":call_id});
        let script = include_str!("browser_voice.js").replace("__CONFIG__", &config.to_string());
        let _ = peer;
        dioxus::prelude::spawn(async move {
            let mut eval = dioxus::document::eval(&script);
            while let Ok(event) = eval.recv::<Event>().await {
                let ended = matches!(event.phase, Phase::Error | Phase::Ended);
                let _ = EVENTS
                    .get_or_init(async_channel::unbounded)
                    .0
                    .try_send(event);
                if ended {
                    break;
                }
            }
        });
        Ok(())
    }
    #[cfg(all(not(target_os = "ios"), not(target_arch = "wasm32")))]
    {
        let _ = (call_id, peer);
        Err("Join the call from Courtside on your iPhone.".into())
    }
}

/// Empty ID ends any local participation, used when signing out.
pub fn end(call_id: Option<Uuid>) {
    #[cfg(target_os = "ios")]
    {
        let id =
            std::ffi::CString::new(call_id.map(|id| id.to_string()).unwrap_or_default()).unwrap();
        unsafe {
            courtside_voice_end(id.as_ptr());
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let id = serde_json::to_string(&call_id).unwrap();
        let _ = dioxus::document::eval(&format!(
            "if (!{id} || window.__courtsideVoice?.call_id === {id}) window.__courtsideVoice?.stop();"
        ));
    }
    #[cfg(all(not(target_os = "ios"), not(target_arch = "wasm32")))]
    let _ = call_id;
}

/// Cancel only an unanswered invitation. Native CallKit is authoritative
/// about whether Answer already happened; UI/socket callback order can vary.
pub fn cancel_invitation(call_id: Uuid) {
    #[cfg(target_os = "ios")]
    {
        let id = std::ffi::CString::new(call_id.to_string()).unwrap();
        unsafe {
            courtside_voice_cancel_invitation(id.as_ptr());
        }
    }
    #[cfg(not(target_os = "ios"))]
    let _ = call_id;
}

pub fn set_muted(call_id: Uuid, muted: bool) {
    #[cfg(target_arch = "wasm32")]
    {
        let id = serde_json::to_string(&call_id).unwrap();
        let _ = dioxus::document::eval(&format!(
            "if (window.__courtsideVoice?.call_id === {id}) window.__courtsideVoice.setMuted({muted});"
        ));
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (call_id, muted);
}
