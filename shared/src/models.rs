use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallDirection {
    Inbound,
    Outbound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallStatus {
    Queued,
    Ringing,
    InProgress,
    Completed,
    Failed,
}

/// Who currently has the speaking role on an active call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallMode {
    /// Courtside is speaking and listening through GPT-Live.
    Assistant,
    /// Twilio is ringing the owner's phone; they have not joined yet.
    TakeoverPending,
    /// The account owner is bridged in by phone while Courtside observes.
    Takeover,
    /// The account owner speaks through the app's native CallKit connection.
    InAppTakeover,
}

/// Voice/model pipeline used for an assistant call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CallAiBackend {
    /// OpenAI GPT-Live full-duplex voice.
    #[default]
    OpenAiLive,
}

/// Built-in GPT Live voice used for assistant speech.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CallVoice {
    #[default]
    Marin,
    Cedar,
    Coral,
    Sage,
    Verse,
    Willow,
}

impl CallVoice {
    pub const fn as_api_name(self) -> &'static str {
        match self {
            Self::Marin => "marin",
            Self::Cedar => "cedar",
            Self::Coral => "coral",
            Self::Sage => "sage",
            Self::Verse => "verse",
            Self::Willow => "willow",
        }
    }

    pub fn from_api_name(value: &str) -> Self {
        match value {
            "cedar" => Self::Cedar,
            "coral" => Self::Coral,
            "sage" => Self::Sage,
            "verse" => Self::Verse,
            "willow" => Self::Willow,
            _ => Self::Marin,
        }
    }
}

/// Who produced a transcript segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Speaker {
    /// The Courtside AI assistant on the call.
    Assistant,
    /// The remote party (person or IVR) on the phone.
    Callee,
    /// The Courtside user, once jump-in exists.
    User,
    /// A non-spoken action performed during the call, such as an IVR keypress.
    Action,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Call {
    pub id: Uuid,
    pub direction: CallDirection,
    /// E.164 number of the remote party.
    pub peer_number: String,
    /// What the user asked the assistant to accomplish on this call.
    pub task_prompt: Option<String>,
    #[serde(default)]
    pub ai_backend: CallAiBackend,
    pub status: CallStatus,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub summary: Option<String>,
    /// Connected, billable call time finalized by the server.
    pub connected_seconds: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptSegment {
    pub id: Uuid,
    pub call_id: Uuid,
    pub speaker: Speaker,
    pub text: String,
    pub at: DateTime<Utc>,
}
