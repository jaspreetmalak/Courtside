//! Commands for a single app participant on the existing Twilio media stream.
//! Microphone and callee audio travel separately; the app never hears its own
//! microphone or the assistant's queued audio during takeover.

use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

pub type VoiceResult = Result<(), String>;

pub enum VoiceCommand {
    DeclineHandoff {
        request_id: Uuid,
        reply: oneshot::Sender<VoiceResult>,
    },
    Join {
        session: Uuid,
        output: mpsc::Sender<VoiceOutput>,
        reply: oneshot::Sender<VoiceResult>,
    },
    Microphone {
        session: Uuid,
        audio: Vec<u8>,
    },
    Leave {
        /// None is an authenticated Return to AI request from the REST API.
        session: Option<Uuid>,
        reply: Option<oneshot::Sender<VoiceResult>>,
    },
}

pub enum VoiceOutput {
    Audio(Vec<u8>),
    Ended(String),
}

pub struct VoicePeer {
    pub session: Uuid,
    pub output: mpsc::Sender<VoiceOutput>,
}

/// A stale socket must never disconnect the participant who joined afterward.
pub fn owns_session(current: Uuid, requested: Option<Uuid>) -> bool {
    requested.is_none_or(|session| session == current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_disconnect_cannot_end_a_new_takeover() {
        let current = Uuid::new_v4();
        assert!(owns_session(current, Some(current)));
        assert!(owns_session(current, None));
        assert!(!owns_session(current, Some(Uuid::new_v4())));
    }
}
