//! Minimal Twilio REST client: place an outbound call whose answered leg
//! hits our voice webhook (which then bridges it to the media stream).

use anyhow::{bail, Context, Result};
use uuid::Uuid;

use crate::config::Config;
use crate::state::AppState;

pub async fn place_outbound_call(
    config: &Config,
    call_id: Uuid,
    from: &str,
    to: &str,
    time_limit_seconds: i64,
) -> Result<String> {
    let url = format!(
        "https://api.twilio.com/2010-04-01/Accounts/{}/Calls.json",
        config.twilio_account_sid
    );
    let webhook = format!(
        "https://{}/twilio/voice?call_id={}",
        config.public_host, call_id
    );
    let status_callback = format!(
        "https://{}/twilio/status?call_id={}",
        config.public_host, call_id
    );
    let time_limit = time_limit_seconds.max(1).to_string();

    let response = reqwest::Client::new()
        .post(&url)
        .basic_auth(&config.twilio_api_key_sid, Some(&config.twilio_api_secret))
        .form(&[
            ("To", to),
            ("From", from),
            ("Url", webhook.as_str()),
            ("StatusCallback", status_callback.as_str()),
            ("StatusCallbackMethod", "POST"),
            ("StatusCallbackEvent", "completed"),
            ("TimeLimit", time_limit.as_str()),
        ])
        .send()
        .await
        .context("twilio request failed")?;

    let status = response.status();
    let body: serde_json::Value = response.json().await.context("twilio response not json")?;
    if !status.is_success() {
        bail!("twilio call creation failed ({status}): {body}");
    }
    body["sid"]
        .as_str()
        .map(str::to_string)
        .context("twilio response missing call sid")
}

pub async fn hangup_call(config: &Config, call_sid: &str) -> Result<()> {
    let base_url = "https://api.twilio.com";
    #[cfg(test)]
    let base_url = config.test_twilio_rest_url.as_deref().unwrap_or(base_url);
    let url = format!(
        "{base_url}/2010-04-01/Accounts/{}/Calls/{call_sid}.json",
        config.twilio_account_sid
    );
    let response = reqwest::Client::new()
        .post(url)
        .timeout(std::time::Duration::from_secs(5))
        .basic_auth(&config.twilio_api_key_sid, Some(&config.twilio_api_secret))
        .form(&[("Status", "completed")])
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("Twilio hangup failed ({})", response.status());
    }
    Ok(())
}

/// Replace the TwiML currently controlling an in-progress Twilio call.
pub async fn redirect_call_to_twiml(config: &Config, call_sid: &str, twiml: &str) -> Result<()> {
    let url = format!(
        "https://api.twilio.com/2010-04-01/Accounts/{}/Calls/{call_sid}.json",
        config.twilio_account_sid
    );
    let response = reqwest::Client::new()
        .post(url)
        .basic_auth(&config.twilio_api_key_sid, Some(&config.twilio_api_secret))
        .form(&[("Twiml", twiml)])
        .send()
        .await
        .context("Twilio call redirect failed")?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("Twilio call redirect failed ({status}): {body}");
    }
    Ok(())
}

/// Status callbacks remain the primary path, but carrier outcomes do not
/// always reach a development tunnel. Polling the authoritative Call resource
/// prevents the app and usage ledger from staying active forever.
pub fn monitor_outbound_call(state: AppState, call_id: Uuid, call_sid: String) {
    tokio::spawn(async move {
        let mut last_status = String::new();
        let mut failures = 0_u32;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let snapshot = match fetch_call(&state.config, &call_sid).await {
                Ok(snapshot) => {
                    failures = 0;
                    snapshot
                }
                Err(err) => {
                    failures += 1;
                    if failures == 5 {
                        tracing::warn!(?err, %call_id, "Twilio call status watcher is retrying");
                    }
                    continue;
                }
            };
            if snapshot.status == last_status {
                continue;
            }
            last_status = snapshot.status.clone();
            match snapshot.status.as_str() {
                "queued" => {
                    state
                        .set_status_and_broadcast(call_id, shared::CallStatus::Queued)
                        .await
                }
                "ringing" => {
                    state
                        .set_status_and_broadcast(call_id, shared::CallStatus::Ringing)
                        .await
                }
                "in-progress" => {
                    state
                        .set_status_and_broadcast(call_id, shared::CallStatus::InProgress)
                        .await
                }
                "completed" | "busy" | "failed" | "no-answer" | "canceled" => {
                    if let Err(err) = crate::db::finalize_call_usage(
                        &state.db,
                        call_id,
                        snapshot.duration_seconds,
                    )
                    .await
                    {
                        tracing::error!(?err, %call_id, "status watcher could not finalize usage");
                    }
                    let status = if snapshot.status == "completed" {
                        shared::CallStatus::Completed
                    } else {
                        shared::CallStatus::Failed
                    };
                    state.set_status_and_broadcast(call_id, status).await;
                    let summary_state = state.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(2_500)).await;
                        if let Err(err) =
                            crate::bridge::call_bridge::summarize_call(&summary_state, call_id)
                                .await
                        {
                            tracing::error!(?err, %call_id, "status watcher could not summarize call");
                        }
                    });
                    break;
                }
                other => {
                    tracing::debug!(%call_id, status = %other, "unhandled Twilio call state")
                }
            }
        }
    });
}

struct TwilioCallSnapshot {
    status: String,
    duration_seconds: i64,
}

async fn fetch_call(config: &Config, call_sid: &str) -> Result<TwilioCallSnapshot> {
    let url = format!(
        "https://api.twilio.com/2010-04-01/Accounts/{}/Calls/{call_sid}.json",
        config.twilio_account_sid
    );
    let response = reqwest::Client::new()
        .get(url)
        .basic_auth(&config.twilio_api_key_sid, Some(&config.twilio_api_secret))
        .send()
        .await
        .context("Twilio call status request failed")?;
    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .await
        .context("Twilio call status response was not JSON")?;
    if !status.is_success() {
        bail!("Twilio call status failed ({status}): {body}");
    }
    Ok(TwilioCallSnapshot {
        status: body["status"].as_str().unwrap_or_default().to_string(),
        duration_seconds: body["duration"]
            .as_str()
            .and_then(|duration| duration.parse().ok())
            .or_else(|| body["duration"].as_i64())
            .unwrap_or_default(),
    })
}
