use chrono::{Duration, Utc};

use crate::state::AppState;

pub fn start(state: AppState) {
    tokio::spawn(async move {
        loop {
            if let Err(err) = reconcile(&state).await {
                tracing::error!(?err, "number lifecycle reconciliation failed");
            }
            tokio::time::sleep(std::time::Duration::from_secs(60 * 60)).await;
        }
    });
}

async fn reconcile(state: &AppState) -> anyhow::Result<()> {
    let numbers = crate::db::releasable_numbers(&state.db).await?;
    let active_pool_count = numbers
        .iter()
        .filter(|number| number.purpose == "trial_pool")
        .count();
    let mut pool_seen = 0usize;
    for number in numbers {
        if crate::db::number_has_active_call(&state.db, number.id).await? {
            continue;
        }
        let should_release = if number.purpose == "dedicated" {
            match number.dedicated_user_id {
                Some(user_id) => crate::db::current_subscription(&state.db, user_id)
                    .await?
                    .is_some_and(|subscription| {
                        subscription.period_end <= Utc::now()
                            || !matches!(subscription.status.as_str(), "active" | "grace_period")
                    }),
                None => true,
            }
        } else {
            pool_seen += 1;
            let near_renewal = number
                .release_due_at
                .is_some_and(|due| due <= Utc::now() + Duration::hours(24));
            let recently_used = number
                .last_used_at
                .is_some_and(|used| used >= Utc::now() - Duration::days(7));
            near_renewal && (!recently_used || active_pool_count > 1 && pool_seen > 1)
        };
        if should_release {
            crate::api::numbers::release_twilio_number(state, &number.twilio_sid)
                .await
                .map_err(|(_, message)| anyhow::anyhow!(message))?;
            crate::db::mark_number_released(&state.db, number.id).await?;
            tracing::info!(phone = %number.phone_number, purpose = %number.purpose, "released Twilio number");
        } else if number.purpose == "trial_pool"
            && number
                .release_due_at
                .is_some_and(|due| due <= Utc::now() + Duration::hours(24))
        {
            // Twilio renews automatically; track the next decision window.
            crate::db::extend_pool_renewal(&state.db, number.id).await?;
        }
    }
    Ok(())
}
