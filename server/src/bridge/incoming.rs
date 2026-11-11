//! Incoming-call updates are private to the number's owner. A handoff request
//! invites them; it never redirects the call or assumes they accepted it.

use anyhow::{Context, Result};
use shared::{CallHandoffRequest, ServerEvent};
use uuid::Uuid;

use crate::state::AppState;

pub async fn announce(state: &AppState, call_id: Uuid) -> Result<()> {
    let call = crate::db::find_call(&state.db, call_id)
        .await?
        .context("call missing")?;
    let user_id = call.user_id.context("incoming owner missing")?;
    let chat_id =
        crate::db::create_incoming_call_chat(&state.db, user_id, &call.peer_number).await?;
    crate::db::set_call_chat_id(&state.db, call_id, chat_id).await?;
    let content = format!(
        "Incoming call from {}. Courtside is answering and will update you here.",
        call.peer_number
    );
    let message =
        crate::db::add_assistant_chat_update(&state.db, user_id, chat_id, &content).await?;
    state.broadcast(ServerEvent::IncomingCall {
        call: crate::db::call_to_shared(&call),
        chat_id,
    });
    state.broadcast(ServerEvent::ChatMessageAdded { call_id, message });
    Ok(())
}

pub async fn update(
    state: &AppState,
    call_id: Uuid,
    caller_name: &str,
    summary: &str,
    request_handoff: bool,
) -> Result<Option<CallHandoffRequest>> {
    let call = crate::db::find_call(&state.db, call_id)
        .await?
        .context("call missing")?;
    anyhow::ensure!(
        call.direction == "inbound" && call.status == "in_progress",
        "incoming call is not connected"
    );
    let user_id = call.user_id.context("incoming owner missing")?;
    let chat_id = call.chat_id.context("incoming chat missing")?;
    let name = compact(caller_name, 80);
    let summary = compact(summary, 600);
    anyhow::ensure!(!summary.is_empty(), "a factual caller update is required");
    let label = if name.is_empty() {
        call.peer_number.as_str()
    } else {
        name.as_str()
    };
    let content = if request_handoff {
        format!("{label}: {summary}\nThey'd like to speak with you. Join the call, or let Courtside take a message.")
    } else {
        format!("{label}: {summary}")
    };
    if state.incoming_update_is_new(call_id, &content).await {
        let message =
            crate::db::add_assistant_chat_update(&state.db, user_id, chat_id, &content).await?;
        state.remember_incoming_update(call_id, &content).await;
        state.broadcast(ServerEvent::ChatMessageAdded { call_id, message });
    }
    if request_handoff {
        Ok(state.request_call_handoff(call_id, &name, &summary).await)
    } else {
        Ok(None)
    }
}

pub async fn unavailable(state: &AppState, call_id: Uuid) {
    let result = async {
        let call = crate::db::find_call(&state.db, call_id)
            .await?
            .context("call missing")?;
        let message = crate::db::add_assistant_chat_update(
            &state.db,
            call.user_id.context("owner missing")?,
            call.chat_id.context("chat missing")?,
            "You didn't join this call. Courtside will offer to take a message.",
        )
        .await?;
        state.broadcast(ServerEvent::ChatMessageAdded { call_id, message });
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if let Err(error) = result {
        tracing::warn!(?error, %call_id, "failed to publish incoming handoff outcome");
    }
}

fn compact(text: &str, max_chars: usize) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::{CallDirection, CallStatus};

    async fn fixture() -> (AppState, Uuid, Uuid) {
        let db = crate::db::connect("sqlite::memory:").await.unwrap();
        let user = crate::db::create_user(&db, "incoming@example.com", "Owner", "unused")
            .await
            .unwrap();
        let call =
            crate::db::create_call(&db, user.id, CallDirection::Inbound, "+14155550100", None)
                .await
                .unwrap();
        let state = AppState::new(db, crate::config::Config::for_test());
        (state, user.id, call.id)
    }

    #[tokio::test]
    async fn incoming_updates_have_their_own_chat_and_recoverable_invitation() {
        let (state, user_id, call_id) = fixture().await;
        let original_chat = crate::db::ensure_current_chat(&state.db, user_id)
            .await
            .unwrap()
            .id;
        let mut events = state.events.subscribe();
        announce(&state, call_id).await.unwrap();
        let chat_id = match events.recv().await.unwrap() {
            ServerEvent::IncomingCall { call, chat_id } => {
                assert_eq!(call.id, call_id);
                chat_id
            }
            other => panic!("unexpected {other:?}"),
        };
        assert_ne!(chat_id, original_chat);
        assert_eq!(
            crate::db::ensure_current_chat(&state.db, user_id)
                .await
                .unwrap()
                .id,
            original_chat
        );
        state
            .set_status_and_broadcast(call_id, CallStatus::InProgress)
            .await;
        let request = update(&state, call_id, "Sam", "Calling about an appointment", true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(state.call_mode(call_id).await, shared::CallMode::Assistant);
        let owner = crate::db::find_user_by_id(&state.db, user_id)
            .await
            .unwrap()
            .unwrap();
        let detail = crate::api::routes::call_detail(
            axum::extract::State(state.clone()),
            crate::api::auth::AuthedUser(owner),
            axum::extract::Path(call_id),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(detail.chat_id, Some(chat_id));
        assert_eq!(detail.handoff, Some(request));
        let history = crate::db::chat_history_for(&state.db, user_id, chat_id)
            .await
            .unwrap();
        assert_eq!(history.len(), 2);
        assert!(history[1].content.contains("appointment"));
    }

    #[tokio::test]
    async fn duplicate_or_declined_requests_cannot_ring_again_and_end_clears_state() {
        let (state, user_id, call_id) = fixture().await;
        announce(&state, call_id).await.unwrap();
        state
            .set_status_and_broadcast(call_id, CallStatus::InProgress)
            .await;
        let first = update(&state, call_id, "Sam", "Please connect me", true)
            .await
            .unwrap()
            .unwrap();
        let repeated = update(&state, call_id, "Sam", "Please connect me", true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first, repeated);
        assert!(
            !state
                .clear_call_handoff(call_id, Some(Uuid::new_v4()))
                .await
        );
        assert!(state.clear_call_handoff(call_id, Some(first.id)).await);
        assert!(update(&state, call_id, "Sam", "Please connect me", true)
            .await
            .unwrap()
            .is_none());
        let chat = crate::db::find_call(&state.db, call_id)
            .await
            .unwrap()
            .unwrap()
            .chat_id
            .unwrap();
        assert_eq!(
            crate::db::chat_history_for(&state.db, user_id, chat)
                .await
                .unwrap()
                .len(),
            2
        );
        state
            .set_status_and_broadcast(call_id, CallStatus::Completed)
            .await;
        assert!(state.call_handoff(call_id).await.is_none());
        assert!(update(&state, call_id, "Sam", "Please connect me", true)
            .await
            .is_err());
    }
}
