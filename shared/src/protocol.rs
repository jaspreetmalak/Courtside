//! Wire protocol between clients and the Courtside server.
//!
//! REST DTOs plus the event stream sent over `GET /api/ws`.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::models::{
    Call, CallAiBackend, CallMode, CallStatus, CallVoice, Speaker, TranscriptSegment,
};

/// Controls read-only lookup through original conversations. No separate facts are saved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistorySettingsResponse {
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateHistorySettingsRequest {
    pub enabled: bool,
}

/// `POST /api/calls` request body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StartCallRequest {
    /// E.164 number to dial, e.g. "+15551234567".
    pub to: String,
    /// What the assistant should accomplish on the call.
    pub task: String,
    /// Voice pipeline to use once the other party answers.
    #[serde(default)]
    pub ai_backend: CallAiBackend,
}

/// `POST /api/calls` response body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StartCallResponse {
    pub call: Call,
}

/// `GET /api/calls` response body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListCallsResponse {
    pub calls: Vec<Call>,
}

/// `GET /api/calls/:id` response body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallDetailResponse {
    pub call: Call,
    pub transcript: Vec<TranscriptSegment>,
    pub mode: CallMode,
    #[serde(default)]
    pub control_message: String,
    #[serde(default)]
    pub chat_id: Option<Uuid>,
    #[serde(default)]
    pub handoff: Option<CallHandoffRequest>,
    #[serde(default)]
    pub pending_question: Option<CallQuestion>,
    #[serde(default)]
    pub remembering: bool,
}

/// A private question for the owner of a live call. The deadline is shared
/// with clients so reconnects and chat switches cannot restart the wait.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallQuestion {
    pub id: Uuid,
    pub chat_id: Uuid,
    pub question: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallAnswerTarget {
    pub call_id: Uuid,
    pub question_id: Uuid,
}

/// An invitation to join an incoming caller. The assistant keeps the call
/// until an authenticated participant has actually connected its audio.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallHandoffRequest {
    pub id: Uuid,
    pub caller_name: String,
    pub reason: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeclineCallHandoffRequest {
    pub request_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallControlResponse {
    pub call_id: Uuid,
    pub mode: CallMode,
    #[serde(default)]
    pub message: String,
}

/// Events pushed to clients over `GET /api/ws`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    /// Work on one owner's chat request; delivered only to that chat's owner.
    ChatActivity {
        chat_id: Uuid,
        request_id: Uuid,
        activity: Option<ChatActivity>,
    },
    IncomingCall {
        call: Call,
        chat_id: Uuid,
    },
    CallHandoffChanged {
        call_id: Uuid,
        request: Option<CallHandoffRequest>,
    },
    CallRecallChanged {
        call_id: Uuid,
        remembering: bool,
    },
    CallQuestionChanged {
        call_id: Uuid,
        question: Option<CallQuestion>,
    },
    CallStatusChanged {
        call_id: Uuid,
        status: CallStatus,
    },
    Transcript {
        segment: TranscriptSegment,
    },
    /// A not-yet-persisted GPT-Live transcript fragment. Clients accumulate
    /// these in delivery order while the call is connected.
    TranscriptDelta {
        call_id: Uuid,
        speaker: Speaker,
        delta: String,
        start_ms: i64,
        end_ms: i64,
    },
    SummaryReady {
        call_id: Uuid,
        summary: String,
    },
    /// A server-originated assistant message, such as a completed-call update.
    ChatMessageAdded {
        call_id: Uuid,
        message: ChatMessage,
    },
    CallModeChanged {
        call_id: Uuid,
        mode: CallMode,
        #[serde(default)]
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatActivity {
    SearchingChats,
    ReadingChat,
}

// --- Auth ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignupRequest {
    pub email: String,
    pub password: String,
    pub display_name: String,
    #[serde(default)]
    pub voice: CallVoice,
    #[serde(default = "default_true")]
    pub announce_courtside: bool,
    #[serde(default)]
    pub custom_instructions: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserProfile {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallPreferences {
    /// Built-in GPT Live voice used for assistant speech.
    pub voice: CallVoice,
    /// Identify the product and user in the standard outbound greeting.
    pub announce_courtside: bool,
    /// Optional persistent preferences for call tone and behavior.
    pub custom_instructions: String,
    /// E.164 number that Twilio rings when the user takes over a live call.
    pub takeover_phone_number: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateCallPreferencesRequest {
    #[serde(default)]
    pub voice: CallVoice,
    pub announce_courtside: bool,
    pub custom_instructions: String,
    #[serde(default)]
    pub takeover_phone_number: String,
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthResponse {
    pub token: String,
    pub user: UserProfile,
}

// --- Chat ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: Uuid,
    pub role: ChatRole,
    pub content: String,
    pub at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatHistoryResponse {
    pub chat_id: Uuid,
    pub messages: Vec<ChatMessage>,
    /// Most recent call started from this chat, if any. A new-chat boundary
    /// intentionally prevents older transcript cards from leaking in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<CallDetailResponse>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatSummary {
    pub id: Uuid,
    pub title: String,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecentChatsResponse {
    pub chats: Vec<ChatSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendChatRequest {
    pub message: String,
    /// Lets the client match progress to the request it is displaying.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<Uuid>,
    /// Explicitly bind a private answer to its question, even if the user
    /// switches chats or the wait expires while the request is in flight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_answer: Option<CallAnswerTarget>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendChatResponse {
    pub user_message: ChatMessage,
    pub reply: ChatMessage,
    /// Present when the assistant started a real outbound call from chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_call: Option<Call>,
}

// --- Phone numbers ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AvailableNumber {
    pub phone_number: String,
    pub friendly_name: String,
    pub locality: Option<String>,
    pub region: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AvailableNumbersResponse {
    pub numbers: Vec<AvailableNumber>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PurchaseNumberRequest {
    pub phone_number: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OwnedNumber {
    pub phone_number: String,
    pub friendly_name: String,
    /// Dedicated numbers accept inbound calls; temporary pool numbers do not.
    #[serde(default)]
    pub inbound_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OwnedNumbersResponse {
    pub numbers: Vec<OwnedNumber>,
}

// --- Account access, verification, and entitlements ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountTier {
    Free,
    Paid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntitlementStatus {
    FreeTrial,
    Active,
    GracePeriod,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountStatusResponse {
    pub phone_verified: bool,
    pub device_attested: bool,
    pub app_attest_required: bool,
    pub verified_phone_masked: Option<String>,
    pub tier: AccountTier,
    pub entitlement: EntitlementStatus,
    /// Lifetime allowance for a free account.
    pub trial_seconds_limit: i64,
    pub trial_seconds_used: i64,
    /// Allowance for the current paid App Store billing period.
    pub included_seconds_limit: i64,
    pub included_seconds_used: i64,
    pub period_end: Option<chrono::DateTime<chrono::Utc>>,
    pub dedicated_number: Option<OwnedNumber>,
    pub needs_number_claim: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttestationChallengeResponse {
    pub challenge: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceAttestationRequest {
    pub challenge: String,
    pub key_id: String,
    /// Base64 CBOR returned by App Attest. May be empty when this key was
    /// already registered for the current account.
    pub attestation: String,
    /// Base64 token returned by DeviceCheck.
    pub device_token: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhoneVerificationStartRequest {
    /// A real mobile number in E.164 form.
    pub phone_number: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhoneVerificationStartResponse {
    pub sent: bool,
    /// Code the user must enter during Twilio's incoming verification call.
    pub validation_code: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhoneVerificationConfirmRequest {
    pub phone_number: String,
    pub code: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubscriptionSyncRequest {
    /// StoreKit 2's signed transaction JWS. It is verified by the server.
    pub signed_transaction_info: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubscriptionSyncResponse {
    pub account: AccountStatusResponse,
}

/// Messages clients may send over `GET /api/ws` (none needed yet; reserved
/// for jump-in and listen-in controls).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientEvent {
    /// Keepalive.
    Ping,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Speaker;
    use chrono::Utc;

    #[test]
    fn server_event_round_trip() {
        let event = ServerEvent::Transcript {
            segment: TranscriptSegment {
                id: Uuid::new_v4(),
                call_id: Uuid::new_v4(),
                speaker: Speaker::Callee,
                text: "Hello, how can I help you?".into(),
                at: Utc::now(),
            },
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"transcript""#));
        let back: ServerEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn live_transcript_delta_round_trip() {
        let event = ServerEvent::TranscriptDelta {
            call_id: Uuid::new_v4(),
            speaker: Speaker::Action,
            delta: "Pressed 1 for Scheduling.".into(),
            start_ms: 240,
            end_ms: 810,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"transcript_delta""#));
        assert!(json.contains(r#""speaker":"action""#));
        assert_eq!(serde_json::from_str::<ServerEvent>(&json).unwrap(), event);
    }

    #[test]
    fn start_call_request_shape() {
        let json = r#"{"to":"+15551234567","task":"Book a table for two at 7pm"}"#;
        let req: StartCallRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.to, "+15551234567");
    }

    #[test]
    fn signup_preferences_have_safe_backwards_compatible_defaults() {
        let request: SignupRequest = serde_json::from_str(
            r#"{"email":"new@example.com","password":"password","display_name":"New User"}"#,
        )
        .unwrap();
        assert_eq!(request.voice, CallVoice::Marin);
        assert!(request.announce_courtside);
        assert!(request.custom_instructions.is_empty());

        let cedar = serde_json::to_string(&CallVoice::Cedar).unwrap();
        assert_eq!(cedar, r#""cedar""#);
    }
}
