//! Twilio Media Streams wire protocol (bidirectional stream over WebSocket).
//!
//! Twilio sends JSON text frames: `connected`, `start`, `media` (base64
//! G.711 μ-law @ 8kHz), `dtmf`, `mark`, `stop`. We send `media`, `mark`,
//! and `clear` frames back on the same socket.

// Wire-protocol types mirror Twilio's schema in full, even fields the bridge
// doesn't read yet (marks, dtmf digits for future phone-tree support).
#![allow(dead_code)]

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Messages received from Twilio.
#[derive(Debug, Deserialize)]
#[serde(tag = "event", rename_all = "lowercase")]
pub enum TwilioInbound {
    Connected {},
    Start { start: StartMeta },
    Media { media: MediaPayload },
    Dtmf { dtmf: DtmfPayload },
    Mark { mark: MarkPayload },
    Stop {},
}

#[derive(Debug, Deserialize)]
pub struct StartMeta {
    #[serde(rename = "streamSid")]
    pub stream_sid: String,
    #[serde(rename = "callSid")]
    pub call_sid: String,
    #[serde(rename = "customParameters", default)]
    pub custom_parameters: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct MediaPayload {
    /// Base64-encoded μ-law audio.
    pub payload: String,
    #[serde(default)]
    pub track: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DtmfPayload {
    pub digit: String,
}

#[derive(Debug, Deserialize)]
pub struct MarkPayload {
    pub name: String,
}

/// Messages sent to Twilio.
#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "lowercase")]
pub enum TwilioOutbound {
    Media {
        #[serde(rename = "streamSid")]
        stream_sid: String,
        media: OutboundMedia,
    },
    Mark {
        #[serde(rename = "streamSid")]
        stream_sid: String,
        mark: OutboundMark,
    },
    /// Discards audio Twilio has buffered but not yet played (barge-in).
    Clear {
        #[serde(rename = "streamSid")]
        stream_sid: String,
    },
}

#[derive(Debug, Serialize)]
pub struct OutboundMedia {
    /// Base64-encoded μ-law audio.
    pub payload: String,
}

#[derive(Debug, Serialize)]
pub struct OutboundMark {
    pub name: String,
}

impl TwilioOutbound {
    pub fn media(stream_sid: &str, base64_payload: String) -> Self {
        Self::Media {
            stream_sid: stream_sid.to_string(),
            media: OutboundMedia {
                payload: base64_payload,
            },
        }
    }

    pub fn clear(stream_sid: &str) -> Self {
        Self::Clear {
            stream_sid: stream_sid.to_string(),
        }
    }

    pub fn mark(stream_sid: &str, name: &str) -> Self {
        Self::Mark {
            stream_sid: stream_sid.to_string(),
            mark: OutboundMark {
                name: name.to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_start_frame() {
        let json = r#"{
            "event": "start",
            "sequenceNumber": "1",
            "start": {
                "accountSid": "AC123",
                "streamSid": "MZ123",
                "callSid": "CA123",
                "tracks": ["inbound"],
                "customParameters": {"call_id": "8f7e0cbb-9f3a-4d8a-9b57-0d6f9241a52e"},
                "mediaFormat": {"encoding": "audio/x-mulaw", "sampleRate": 8000, "channels": 1}
            },
            "streamSid": "MZ123"
        }"#;
        let TwilioInbound::Start { start } = serde_json::from_str(json).unwrap() else {
            panic!("expected start frame");
        };
        assert_eq!(start.stream_sid, "MZ123");
        assert_eq!(start.call_sid, "CA123");
        assert_eq!(
            start.custom_parameters.get("call_id").unwrap(),
            "8f7e0cbb-9f3a-4d8a-9b57-0d6f9241a52e"
        );
    }

    #[test]
    fn parses_media_frame() {
        let json = r#"{
            "event": "media",
            "sequenceNumber": "3",
            "media": {"track": "inbound", "chunk": "2", "timestamp": "5", "payload": "fn5+fg=="},
            "streamSid": "MZ123"
        }"#;
        let TwilioInbound::Media { media } = serde_json::from_str(json).unwrap() else {
            panic!("expected media frame");
        };
        assert_eq!(media.payload, "fn5+fg==");
    }

    #[test]
    fn serializes_outbound_media_clear_and_mark() {
        let media = TwilioOutbound::media("MZ123", "AAAA".into());
        assert_eq!(
            serde_json::to_string(&media).unwrap(),
            r#"{"event":"media","streamSid":"MZ123","media":{"payload":"AAAA"}}"#
        );
        let clear = TwilioOutbound::clear("MZ123");
        assert_eq!(
            serde_json::to_string(&clear).unwrap(),
            r#"{"event":"clear","streamSid":"MZ123"}"#
        );
        let mark = TwilioOutbound::mark("MZ123", "closing_played");
        assert_eq!(
            serde_json::to_string(&mark).unwrap(),
            r#"{"event":"mark","streamSid":"MZ123","mark":{"name":"closing_played"}}"#
        );
    }
}
