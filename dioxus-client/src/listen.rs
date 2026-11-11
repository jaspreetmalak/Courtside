//! Playback-only native audio; independent of microphone participation.
use crate::voice::Phase;
use serde::Deserialize;
use std::sync::OnceLock;
use uuid::Uuid;

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

fn publish(event: Event) {
    let _ = EVENTS
        .get_or_init(async_channel::unbounded)
        .0
        .try_send(event);
}

#[cfg(target_os = "ios")]
extern "C" {
    fn courtside_listen_start(
        url: *const std::ffi::c_char,
        token: *const std::ffi::c_char,
        call_id: *const std::ffi::c_char,
        callback: extern "C" fn(*const std::ffi::c_char),
    );
    fn courtside_listen_stop(call_id: *const std::ffi::c_char);
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
        publish(event);
    }
}

pub fn start(call_id: Uuid) -> Result<(), String> {
    #[cfg(target_os = "ios")]
    {
        use std::ffi::CString;
        let (url, token) = crate::api::call_listen_connection(call_id)?;
        let strings = [url, token, call_id.to_string()]
            .into_iter()
            .map(CString::new)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "Unable to prepare live audio.".to_string())?;
        unsafe {
            courtside_listen_start(
                strings[0].as_ptr(),
                strings[1].as_ptr(),
                strings[2].as_ptr(),
                receive_event,
            );
        }
        Ok(())
    }
    #[cfg(not(target_os = "ios"))]
    {
        let url = crate::api::call_audio_ws_url(call_id)?;
        let config = serde_json::json!({"url":url, "call_id":call_id});
        let script = include_str!("listen.js").replace("__CONFIG__", &config.to_string());
        dioxus::prelude::spawn(async move {
            let mut eval = dioxus::document::eval(&script);
            while let Ok(event) = eval.recv::<Event>().await {
                publish(event);
            }
        });
        Ok(())
    }
}

pub fn stop(call_id: Option<Uuid>) {
    #[cfg(target_os = "ios")]
    {
        let id =
            std::ffi::CString::new(call_id.map(|id| id.to_string()).unwrap_or_default()).unwrap();
        unsafe {
            courtside_listen_stop(id.as_ptr());
        }
    }
    #[cfg(not(target_os = "ios"))]
    {
        let id = serde_json::to_string(&call_id).unwrap();
        let _ = dioxus::document::eval(&format!(
            "if (!{id} || window.__courtsideListen?.call_id === {id}) window.__courtsideListen?.stop();"
        ));
    }
}
