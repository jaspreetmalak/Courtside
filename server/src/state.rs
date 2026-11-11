use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Instant,
};

use sea_orm::DatabaseConnection;
use shared::{CallHandoffRequest, CallMode, CallQuestion, CallStatus, ServerEvent};
use tokio::sync::{broadcast, mpsc, Mutex};
use uuid::Uuid;

use crate::config::Config;

#[derive(Clone)]
pub struct AppState {
    pub db: DatabaseConnection,
    pub config: Arc<Config>,
    pub events: broadcast::Sender<ServerEvent>,
    pub call_audio: broadcast::Sender<CallAudioFrame>,
    voice_relays:
        Arc<Mutex<HashMap<Uuid, (Uuid, mpsc::Sender<crate::bridge::voice::VoiceCommand>)>>>,
    call_controls: Arc<Mutex<HashMap<Uuid, CallControlState>>>,
    /// Serialize manual redirects so rapid takeover/return taps cannot race.
    call_control_locks: Arc<Mutex<HashMap<Uuid, Arc<Mutex<()>>>>>,
    pub summaries_started: Arc<Mutex<HashSet<Uuid>>>,
    pub takeover_contexts: Arc<Mutex<HashMap<Uuid, String>>>,
    /// Private text answers from the app that should be injected into a
    /// currently connected GPT-Live call.
    live_call_inputs: Arc<Mutex<HashMap<Uuid, mpsc::UnboundedSender<LiveCallAnswer>>>>,
    /// Calls currently blocked on one precise piece of user-owned information.
    pending_call_questions: Arc<Mutex<HashMap<Uuid, PendingCallQuestion>>>,
    call_question_timeouts: Arc<Mutex<HashSet<Uuid>>>,
    remembering_calls: Arc<Mutex<HashSet<Uuid>>>,
    incoming_calls: Arc<Mutex<HashMap<Uuid, IncomingCallState>>>,
    /// Serializes trial-pool provisioning so concurrent first calls buy once.
    pub number_provisioning: Arc<Mutex<()>>,
    pub call_start: Arc<Mutex<()>>,
    /// In-flight Twilio OutgoingCallerId challenges, scoped to their account.
    pub caller_id_verifications: Arc<Mutex<HashMap<Uuid, PendingCallerIdVerification>>>,
}

#[derive(Clone, Debug)]
pub struct CallAudioFrame {
    pub call_id: Uuid,
    pub track: AudioTrack,
    pub emitted_at: Instant,
    /// Base64-encoded 8 kHz G.711 mu-law audio, directly from Twilio/GPT-Live.
    /// An empty payload clears this track's buffered audio.
    pub payload: String,
}

// First byte of the native listen websocket's binary frames. Keep these values
// in sync with ListenTrack in the iOS player; the rest of a frame is G.711.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum AudioTrack {
    Callee = 1,
    Assistant = 2,
    User = 3,
}

#[derive(Clone, Debug)]
pub struct PendingCallerIdVerification {
    pub phone_number: String,
    pub validation_code: String,
    pub started_at: Instant,
}

#[derive(Clone)]
struct CallControlState {
    mode: CallMode,
    message: String,
    takeover_attempt: Option<Uuid>,
}

#[derive(Default)]
struct IncomingCallState {
    last_update: String,
    handoff_offered: bool,
    handoff: Option<CallHandoffRequest>,
}

pub const CALL_QUESTION_WAIT_SECONDS: u64 = 20;

#[derive(Clone, Debug)]
pub struct PendingCallQuestion {
    pub request: CallQuestion,
    // Server deadlines use a monotonic clock; the wire timestamp is for UI.
    pub deadline: tokio::time::Instant,
}

impl PendingCallQuestion {
    pub fn remaining_seconds(&self) -> u64 {
        self.deadline
            .saturating_duration_since(tokio::time::Instant::now())
            .as_millis()
            .div_ceil(1_000) as u64
    }
}

#[derive(Clone, Debug)]
pub struct LiveCallAnswer {
    pub question: CallQuestion,
    pub answer: String,
}

impl AppState {
    pub fn new(db: DatabaseConnection, config: Config) -> Self {
        let (events, _) = broadcast::channel(256);
        let (call_audio, _) = broadcast::channel(2_048);
        Self {
            db,
            config: Arc::new(config),
            events,
            call_audio,
            voice_relays: Arc::new(Mutex::new(HashMap::new())),
            call_controls: Arc::new(Mutex::new(HashMap::new())),
            call_control_locks: Arc::new(Mutex::new(HashMap::new())),
            summaries_started: Arc::new(Mutex::new(HashSet::new())),
            takeover_contexts: Arc::new(Mutex::new(HashMap::new())),
            live_call_inputs: Arc::new(Mutex::new(HashMap::new())),
            pending_call_questions: Arc::new(Mutex::new(HashMap::new())),
            call_question_timeouts: Arc::new(Mutex::new(HashSet::new())),
            remembering_calls: Arc::new(Mutex::new(HashSet::new())),
            incoming_calls: Arc::new(Mutex::new(HashMap::new())),
            number_provisioning: Arc::new(Mutex::new(())),
            call_start: Arc::new(Mutex::new(())),
            caller_id_verifications: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn broadcast(&self, event: ServerEvent) {
        // Only fails when no client is connected, which is fine.
        let _ = self.events.send(event);
    }

    pub fn broadcast_audio(&self, call_id: Uuid, track: AudioTrack, payload: &str) {
        let _ = self.call_audio.send(CallAudioFrame {
            call_id,
            track,
            emitted_at: Instant::now(),
            payload: payload.to_string(),
        });
    }

    pub async fn register_voice_relay(
        &self,
        call_id: Uuid,
        sender: mpsc::Sender<crate::bridge::voice::VoiceCommand>,
    ) -> Uuid {
        let bridge = Uuid::new_v4();
        self.voice_relays
            .lock()
            .await
            .insert(call_id, (bridge, sender));
        bridge
    }

    pub async fn voice_relay(
        &self,
        call_id: Uuid,
    ) -> Option<mpsc::Sender<crate::bridge::voice::VoiceCommand>> {
        self.voice_relays
            .lock()
            .await
            .get(&call_id)
            .filter(|(_, sender)| !sender.is_closed())
            .map(|(_, sender)| sender.clone())
    }

    pub async fn unregister_voice_relay(&self, call_id: Uuid, bridge: Uuid) {
        let mut relays = self.voice_relays.lock().await;
        if relays
            .get(&call_id)
            .is_some_and(|(current, _)| *current == bridge)
        {
            relays.remove(&call_id);
        }
    }

    pub async fn call_mode(&self, call_id: Uuid) -> CallMode {
        self.call_controls
            .lock()
            .await
            .get(&call_id)
            .map(|control| control.mode)
            .unwrap_or(CallMode::Assistant)
    }

    pub async fn call_control_lock(&self, call_id: Uuid) -> Arc<Mutex<()>> {
        self.call_control_locks
            .lock()
            .await
            .entry(call_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    pub async fn set_call_mode(&self, call_id: Uuid, mode: CallMode) {
        self.set_call_control(call_id, mode, "").await;
    }

    pub async fn call_control(&self, call_id: Uuid) -> shared::CallControlResponse {
        let controls = self.call_controls.lock().await;
        let control = controls.get(&call_id);
        shared::CallControlResponse {
            call_id,
            mode: control
                .map(|control| control.mode)
                .unwrap_or(CallMode::Assistant),
            message: control
                .map(|control| control.message.clone())
                .unwrap_or_default(),
        }
    }

    pub async fn set_call_control(&self, call_id: Uuid, mode: CallMode, message: &str) {
        let mut controls = self.call_controls.lock().await;
        controls.insert(
            call_id,
            CallControlState {
                mode,
                message: message.into(),
                takeover_attempt: None,
            },
        );
        self.broadcast(ServerEvent::CallModeChanged {
            call_id,
            mode,
            message: message.into(),
        });
    }

    pub async fn begin_takeover(&self, call_id: Uuid) -> Uuid {
        let attempt = Uuid::new_v4();
        let message = "Calling your takeover number. Answer the incoming call to join.";
        self.call_controls.lock().await.insert(
            call_id,
            CallControlState {
                mode: CallMode::TakeoverPending,
                message: message.into(),
                takeover_attempt: Some(attempt),
            },
        );
        self.broadcast(ServerEvent::CallModeChanged {
            call_id,
            mode: CallMode::TakeoverPending,
            message: message.into(),
        });
        attempt
    }

    pub async fn takeover_attempt(&self, call_id: Uuid) -> Option<Uuid> {
        self.call_controls
            .lock()
            .await
            .get(&call_id)
            .and_then(|control| control.takeover_attempt)
    }

    /// Ignore late callbacks from a canceled or previous takeover attempt.
    pub async fn update_takeover(
        &self,
        call_id: Uuid,
        attempt: Uuid,
        mode: CallMode,
        message: &str,
    ) -> bool {
        let mut controls = self.call_controls.lock().await;
        let Some(control) = controls
            .get_mut(&call_id)
            .filter(|control| control.takeover_attempt == Some(attempt))
        else {
            return false;
        };
        // Twilio delivers asynchronous status callbacks out of order.
        if control.mode == CallMode::Takeover && mode == CallMode::TakeoverPending {
            return false;
        }
        control.mode = mode;
        control.message = message.into();
        if mode == CallMode::Assistant {
            control.takeover_attempt = None;
        }
        self.broadcast(ServerEvent::CallModeChanged {
            call_id,
            mode,
            message: message.into(),
        });
        true
    }

    pub async fn begin_summary(&self, call_id: Uuid) -> bool {
        self.summaries_started.lock().await.insert(call_id)
    }

    pub async fn append_takeover_context(
        &self,
        call_id: Uuid,
        speaker: shared::Speaker,
        text: &str,
    ) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let label = match speaker {
            shared::Speaker::Assistant => "Courtside",
            shared::Speaker::Callee => "Other party",
            shared::Speaker::User => "User",
            shared::Speaker::Action => "Call action",
        };
        let mut contexts = self.takeover_contexts.lock().await;
        let context = contexts.entry(call_id).or_default();
        context.push_str(label);
        context.push_str(": ");
        context.push_str(text);
        context.push('\n');
    }

    pub async fn takeover_context(&self, call_id: Uuid) -> String {
        self.takeover_contexts
            .lock()
            .await
            .get(&call_id)
            .cloned()
            .unwrap_or_default()
    }

    pub async fn clear_takeover_context(&self, call_id: Uuid) {
        self.takeover_contexts.lock().await.remove(&call_id);
    }

    pub async fn register_live_call_input(
        &self,
        call_id: Uuid,
        sender: mpsc::UnboundedSender<LiveCallAnswer>,
    ) {
        self.live_call_inputs.lock().await.insert(call_id, sender);
    }

    pub async fn unregister_live_call_input(&self, call_id: Uuid) {
        self.live_call_inputs.lock().await.remove(&call_id);
        self.clear_call_question(call_id).await;
    }

    pub async fn incoming_update_is_new(&self, call_id: Uuid, update: &str) -> bool {
        self.incoming_calls
            .lock()
            .await
            .get(&call_id)
            .is_none_or(|incoming| incoming.last_update != update)
    }

    pub async fn remember_incoming_update(&self, call_id: Uuid, update: &str) {
        self.incoming_calls
            .lock()
            .await
            .entry(call_id)
            .or_default()
            .last_update = update.into();
    }

    pub async fn call_handoff(&self, call_id: Uuid) -> Option<CallHandoffRequest> {
        self.incoming_calls
            .lock()
            .await
            .get(&call_id)?
            .handoff
            .clone()
    }

    /// One invitation per call avoids repeatedly ringing an owner who declined
    /// or missed it. They can still join manually from the live call card.
    pub async fn request_call_handoff(
        &self,
        call_id: Uuid,
        caller_name: &str,
        reason: &str,
    ) -> Option<CallHandoffRequest> {
        let mut calls = self.incoming_calls.lock().await;
        let incoming = calls.entry(call_id).or_default();
        if incoming.handoff_offered {
            return incoming.handoff.clone();
        }
        let request = CallHandoffRequest {
            id: Uuid::new_v4(),
            caller_name: caller_name.into(),
            reason: reason.into(),
            expires_at: chrono::Utc::now() + chrono::Duration::seconds(45),
        };
        incoming.handoff_offered = true;
        incoming.handoff = Some(request.clone());
        self.broadcast(ServerEvent::CallHandoffChanged {
            call_id,
            request: Some(request.clone()),
        });
        Some(request)
    }

    pub async fn clear_call_handoff(&self, call_id: Uuid, request_id: Option<Uuid>) -> bool {
        let mut calls = self.incoming_calls.lock().await;
        let Some(incoming) = calls.get_mut(&call_id) else {
            return false;
        };
        let matches = incoming
            .handoff
            .as_ref()
            .is_some_and(|request| request_id.is_none_or(|id| request.id == id));
        if !matches {
            return false;
        }
        incoming.handoff = None;
        self.broadcast(ServerEvent::CallHandoffChanged {
            call_id,
            request: None,
        });
        true
    }

    pub async fn set_call_remembering(&self, call_id: Uuid, remembering: bool) {
        let mut calls = self.remembering_calls.lock().await;
        let changed = if remembering {
            calls.insert(call_id)
        } else {
            calls.remove(&call_id)
        };
        if changed {
            self.broadcast(ServerEvent::CallRecallChanged {
                call_id,
                remembering,
            });
        }
    }

    pub async fn call_remembering(&self, call_id: Uuid) -> bool {
        self.remembering_calls.lock().await.contains(&call_id)
    }

    /// A reply already accepted from the owner wins over a history result.
    pub async fn resolve_call_question_from_history(
        &self,
        call_id: Uuid,
        question_id: Uuid,
    ) -> bool {
        let mut questions = self.pending_call_questions.lock().await;
        if !questions.get(&call_id).is_some_and(|pending| {
            pending.request.id == question_id && pending.deadline > tokio::time::Instant::now()
        }) {
            return false;
        }
        questions.remove(&call_id);
        self.broadcast(ServerEvent::CallQuestionChanged {
            call_id,
            question: None,
        });
        true
    }

    pub async fn call_is_waiting_for_user(&self, call_id: Uuid) -> bool {
        self.pending_call_questions
            .lock()
            .await
            .contains_key(&call_id)
    }

    pub async fn call_question(&self, call_id: Uuid) -> Option<CallQuestion> {
        self.pending_call_question(call_id)
            .await
            .map(|pending| pending.request)
    }

    pub async fn pending_call_question(&self, call_id: Uuid) -> Option<PendingCallQuestion> {
        self.pending_call_questions
            .lock()
            .await
            .get(&call_id)
            .cloned()
    }

    pub async fn call_question_timed_out(&self, call_id: Uuid) -> bool {
        self.call_question_timeouts.lock().await.contains(&call_id)
    }

    pub async fn set_pending_call_question(
        &self,
        call_id: Uuid,
        chat_id: Uuid,
        question: &str,
    ) -> PendingCallQuestion {
        let mut questions = self.pending_call_questions.lock().await;
        if let Some(existing) = questions.get(&call_id) {
            return existing.clone();
        }
        let pending = PendingCallQuestion {
            request: CallQuestion {
                id: Uuid::new_v4(),
                chat_id,
                question: question.into(),
                expires_at: chrono::Utc::now()
                    + chrono::Duration::seconds(CALL_QUESTION_WAIT_SECONDS as i64),
            },
            deadline: tokio::time::Instant::now()
                + std::time::Duration::from_secs(CALL_QUESTION_WAIT_SECONDS),
        };
        questions.insert(call_id, pending.clone());
        self.broadcast(ServerEvent::CallQuestionChanged {
            call_id,
            question: Some(pending.request.clone()),
        });
        pending
    }

    pub async fn clear_call_question(&self, call_id: Uuid) {
        self.set_call_remembering(call_id, false).await;
        if self
            .pending_call_questions
            .lock()
            .await
            .remove(&call_id)
            .is_some()
        {
            self.broadcast(ServerEvent::CallQuestionChanged {
                call_id,
                question: None,
            });
        }
    }

    /// Atomically arbitrate an answer against its deadline. An answer already
    /// accepted by the relay wins over timeout; a later answer cannot leak into
    /// a replacement question or a separate planning turn.
    pub async fn expire_call_question(&self, call_id: Uuid, question_id: Uuid) -> bool {
        let mut questions = self.pending_call_questions.lock().await;
        let expired = questions.get(&call_id).is_some_and(|pending| {
            pending.request.id == question_id && pending.deadline <= tokio::time::Instant::now()
        });
        if !expired {
            return false;
        }
        questions.remove(&call_id);
        self.set_call_remembering(call_id, false).await;
        self.call_question_timeouts.lock().await.insert(call_id);
        self.broadcast(ServerEvent::CallQuestionChanged {
            call_id,
            question: None,
        });
        true
    }

    pub async fn deliver_call_answer(
        &self,
        call_id: Uuid,
        question_id: Uuid,
        answer: String,
    ) -> bool {
        let sender = self.live_call_inputs.lock().await.get(&call_id).cloned();
        let mut questions = self.pending_call_questions.lock().await;
        let Some(pending) = questions.get(&call_id) else {
            return false;
        };
        if pending.request.id != question_id || pending.deadline <= tokio::time::Instant::now() {
            return false;
        }
        let delivered = sender.is_some_and(|sender| {
            sender
                .send(LiveCallAnswer {
                    question: pending.request.clone(),
                    answer,
                })
                .is_ok()
        });
        questions.remove(&call_id);
        self.set_call_remembering(call_id, false).await;
        self.broadcast(ServerEvent::CallQuestionChanged {
            call_id,
            question: None,
        });
        delivered
    }

    pub async fn set_status_and_broadcast(&self, call_id: Uuid, status: CallStatus) {
        if let Err(err) = crate::db::set_call_status(&self.db, call_id, status).await {
            tracing::error!(?err, %call_id, "failed to update call status");
            return;
        }
        if matches!(status, CallStatus::Completed | CallStatus::Failed) {
            self.unregister_live_call_input(call_id).await;
            self.call_question_timeouts.lock().await.remove(&call_id);
            self.clear_call_handoff(call_id, None).await;
            self.incoming_calls.lock().await.remove(&call_id);
        }
        self.broadcast(ServerEvent::CallStatusChanged { call_id, status });
    }

    pub async fn account_status(
        &self,
        user_id: Uuid,
    ) -> anyhow::Result<shared::AccountStatusResponse> {
        let mut status = crate::db::account_status(&self.db, user_id).await?;
        status.app_attest_required = self.config.require_app_attest;
        if self.config.dev_bypass_phone_verification {
            status.phone_verified = true;
            status.verified_phone_masked = Some("Development bypass".into());
        }
        if self.config.dev_bypass_usage_limits {
            status.trial_seconds_used = 0;
            status.included_seconds_used = 0;
            if status.tier == shared::AccountTier::Free {
                status.entitlement = shared::EntitlementStatus::FreeTrial;
            }
        }
        Ok(status)
    }
}
