pub mod entities;
pub mod history;
mod history_migration;
pub mod history_settings;
pub mod migration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, Database, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, TransactionTrait,
};
use sea_orm_migration::MigratorTrait;
use shared::{Call, CallAiBackend, CallDirection, CallStatus, Speaker, TranscriptSegment};
use uuid::Uuid;

use entities::{
    attestation_challenges, auth_tokens, calls, chat_messages, chat_sessions, device_attestations,
    phone_numbers, subscriptions, transcript_segments, usage_records, users, verified_phones,
};
use shared::{
    AccountStatusResponse, AccountTier, CallPreferences, CallVoice, ChatMessage, ChatRole,
    ChatSummary, EntitlementStatus, OwnedNumber, UserProfile,
};

pub const TRIAL_SECONDS_LIMIT: i64 = 15 * 60;
pub const PAID_SECONDS_LIMIT: i64 = 120 * 60;

pub async fn connect(database_url: &str) -> Result<DatabaseConnection> {
    let db = Database::connect(database_url).await?;
    migration::Migrator::up(&db, None).await?;
    Ok(db)
}

// Enum <-> string mapping for storage. Serde's snake_case names are the
// canonical wire spelling, so reuse them for the DB too.
fn to_str<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .unwrap()
        .as_str()
        .unwrap()
        .to_string()
}

fn from_str<T: serde::de::DeserializeOwned>(value: &str) -> T {
    serde_json::from_value(serde_json::Value::String(value.to_string())).unwrap()
}

pub fn call_to_shared(model: &calls::Model) -> Call {
    Call {
        id: model.id,
        direction: from_str(&model.direction),
        peer_number: model.peer_number.clone(),
        task_prompt: model.task_prompt.clone(),
        // Historical experimental-provider rows are exposed as the only
        // currently supported provider. The stored column stays generic so
        // future hosted providers can be added without another schema churn.
        ai_backend: CallAiBackend::OpenAiLive,
        status: from_str(&model.status),
        started_at: model.started_at,
        ended_at: model.ended_at,
        summary: model.summary.clone(),
        connected_seconds: model.connected_seconds,
        created_at: model.created_at,
    }
}

pub fn segment_to_shared(model: &transcript_segments::Model) -> TranscriptSegment {
    TranscriptSegment {
        id: model.id,
        call_id: model.call_id,
        speaker: from_str(&model.speaker),
        text: model.text.clone(),
        at: model.at,
    }
}

pub async fn create_call(
    db: &DatabaseConnection,
    user_id: Uuid,
    direction: CallDirection,
    peer_number: &str,
    task_prompt: Option<&str>,
) -> Result<Call> {
    create_metered_call(
        db,
        user_id,
        direction,
        peer_number,
        task_prompt,
        None,
        None,
        None,
        true,
        true,
        CallAiBackend::OpenAiLive,
    )
    .await
}

pub async fn create_metered_call(
    db: &DatabaseConnection,
    user_id: Uuid,
    direction: CallDirection,
    peer_number: &str,
    task_prompt: Option<&str>,
    from_number_id: Option<Uuid>,
    usage_bucket: Option<&str>,
    quota_cap_seconds: Option<i64>,
    announce_courtside: bool,
    share_user_name: bool,
    ai_backend: CallAiBackend,
) -> Result<Call> {
    let model = calls::ActiveModel {
        id: Set(Uuid::new_v4()),
        user_id: Set(Some(user_id)),
        chat_id: Set(None),
        direction: Set(to_str(&direction)),
        peer_number: Set(peer_number.to_string()),
        task_prompt: Set(task_prompt.map(str::to_string)),
        ai_backend: Set(to_str(&ai_backend)),
        status: Set(to_str(&CallStatus::Queued)),
        twilio_call_sid: Set(None),
        started_at: Set(None),
        ended_at: Set(None),
        summary: Set(None),
        from_number_id: Set(from_number_id),
        usage_bucket: Set(usage_bucket.map(str::to_string)),
        quota_cap_seconds: Set(quota_cap_seconds),
        connected_seconds: Set(0),
        usage_finalized: Set(false),
        announce_courtside: Set(announce_courtside),
        share_user_name: Set(share_user_name),
        created_at: Set(Utc::now()),
    }
    .insert(db)
    .await?;
    Ok(call_to_shared(&model))
}

pub async fn find_call(db: &DatabaseConnection, id: Uuid) -> Result<Option<calls::Model>> {
    Ok(calls::Entity::find_by_id(id).one(db).await?)
}

pub async fn find_call_by_sid(db: &DatabaseConnection, sid: &str) -> Result<Option<calls::Model>> {
    Ok(calls::Entity::find()
        .filter(calls::Column::TwilioCallSid.eq(sid))
        .one(db)
        .await?)
}

pub async fn find_call_for_user(
    db: &DatabaseConnection,
    id: Uuid,
    user_id: Uuid,
) -> Result<Option<calls::Model>> {
    Ok(calls::Entity::find_by_id(id)
        .filter(calls::Column::UserId.eq(user_id))
        .one(db)
        .await?)
}

pub async fn list_calls(db: &DatabaseConnection, user_id: Uuid) -> Result<Vec<Call>> {
    let models = calls::Entity::find()
        .filter(calls::Column::UserId.eq(user_id))
        .order_by_desc(calls::Column::CreatedAt)
        .all(db)
        .await?;
    Ok(models.iter().map(call_to_shared).collect())
}

pub async fn call_belongs_to_user(
    db: &DatabaseConnection,
    call_id: Uuid,
    user_id: Uuid,
) -> Result<bool> {
    Ok(find_call_for_user(db, call_id, user_id).await?.is_some())
}

pub async fn call_transcript(
    db: &DatabaseConnection,
    call_id: Uuid,
) -> Result<Vec<TranscriptSegment>> {
    let models = transcript_segments::Entity::find()
        .filter(transcript_segments::Column::CallId.eq(call_id))
        .order_by_asc(transcript_segments::Column::At)
        .all(db)
        .await?;
    Ok(models.iter().map(segment_to_shared).collect())
}

pub async fn set_call_sid(db: &DatabaseConnection, id: Uuid, sid: &str) -> Result<()> {
    let mut active: calls::ActiveModel = match find_call(db, id).await? {
        Some(model) => model.into(),
        None => return Ok(()),
    };
    active.twilio_call_sid = Set(Some(sid.to_string()));
    active.update(db).await?;
    Ok(())
}

pub async fn set_call_status(db: &DatabaseConnection, id: Uuid, status: CallStatus) -> Result<()> {
    let mut active: calls::ActiveModel = match find_call(db, id).await? {
        Some(model) => model.into(),
        None => return Ok(()),
    };
    match status {
        CallStatus::InProgress => active.started_at = Set(Some(Utc::now())),
        CallStatus::Completed | CallStatus::Failed => active.ended_at = Set(Some(Utc::now())),
        _ => {}
    }
    active.status = Set(to_str(&status));
    active.update(db).await?;
    Ok(())
}

pub async fn set_call_summary(db: &DatabaseConnection, id: Uuid, summary: &str) -> Result<()> {
    let mut active: calls::ActiveModel = match find_call(db, id).await? {
        Some(model) => model.into(),
        None => return Ok(()),
    };
    active.summary = Set(Some(summary.to_string()));
    active.update(db).await?;
    Ok(())
}

pub async fn add_segment(
    db: &DatabaseConnection,
    call_id: Uuid,
    speaker: Speaker,
    text: &str,
) -> Result<TranscriptSegment> {
    let model = transcript_segments::ActiveModel {
        id: Set(Uuid::new_v4()),
        call_id: Set(call_id),
        speaker: Set(to_str(&speaker)),
        text: Set(text.to_string()),
        at: Set(Utc::now()),
    }
    .insert(db)
    .await?;
    Ok(segment_to_shared(&model))
}

// --- Users & auth ---

pub fn user_to_profile(model: &users::Model) -> UserProfile {
    UserProfile {
        id: model.id,
        email: model.email.clone(),
        display_name: model.display_name.clone(),
    }
}

pub async fn create_user(
    db: &DatabaseConnection,
    email: &str,
    display_name: &str,
    password_hash: &str,
) -> Result<users::Model> {
    create_user_with_preferences(
        db,
        email,
        display_name,
        password_hash,
        CallVoice::default(),
        true,
        None,
    )
    .await
}

pub async fn create_user_with_preferences(
    db: &DatabaseConnection,
    email: &str,
    display_name: &str,
    password_hash: &str,
    voice: CallVoice,
    announce_courtside: bool,
    custom_instructions: Option<&str>,
) -> Result<users::Model> {
    let user = users::ActiveModel {
        id: Set(Uuid::new_v4()),
        email: Set(email.to_lowercase()),
        display_name: Set(display_name.to_string()),
        password_hash: Set(password_hash.to_string()),
        voice_preference: Set(to_str(&voice)),
        announce_courtside: Set(announce_courtside),
        custom_call_instructions: Set(custom_instructions.map(str::to_string)),
        takeover_phone_number: Set(None),
        current_chat_id: Set(None),
        created_at: Set(Utc::now()),
    }
    .insert(db)
    .await?;
    let chat = create_chat_session(db, user.id).await?;
    let mut active: users::ActiveModel = user.into();
    active.current_chat_id = Set(Some(chat.id));
    Ok(active.update(db).await?)
}

pub async fn find_user_by_email(
    db: &DatabaseConnection,
    email: &str,
) -> Result<Option<users::Model>> {
    Ok(users::Entity::find()
        .filter(users::Column::Email.eq(email.to_lowercase()))
        .one(db)
        .await?)
}

pub async fn find_user_by_id(
    db: &DatabaseConnection,
    user_id: Uuid,
) -> Result<Option<users::Model>> {
    Ok(users::Entity::find_by_id(user_id).one(db).await?)
}

pub fn call_preferences(user: &users::Model) -> CallPreferences {
    CallPreferences {
        voice: serde_json::from_value(serde_json::Value::String(user.voice_preference.clone()))
            .unwrap_or_default(),
        announce_courtside: user.announce_courtside,
        custom_instructions: user.custom_call_instructions.clone().unwrap_or_default(),
        takeover_phone_number: user.takeover_phone_number.clone().unwrap_or_default(),
    }
}

pub async fn update_call_preferences(
    db: &DatabaseConnection,
    user: users::Model,
    voice: CallVoice,
    announce_courtside: bool,
    custom_instructions: Option<&str>,
    takeover_phone_number: Option<&str>,
) -> Result<CallPreferences> {
    let mut active: users::ActiveModel = user.into();
    active.voice_preference = Set(to_str(&voice));
    active.announce_courtside = Set(announce_courtside);
    active.custom_call_instructions = Set(custom_instructions.map(str::to_string));
    active.takeover_phone_number = Set(takeover_phone_number.map(str::to_string));
    let updated = active.update(db).await?;
    Ok(call_preferences(&updated))
}

pub async fn insert_token(db: &DatabaseConnection, user_id: Uuid) -> Result<String> {
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    auth_tokens::ActiveModel {
        token: Set(token.clone()),
        user_id: Set(user_id),
        created_at: Set(Utc::now()),
    }
    .insert(db)
    .await?;
    Ok(token)
}

pub async fn user_for_token(db: &DatabaseConnection, token: &str) -> Result<Option<users::Model>> {
    let Some(row) = auth_tokens::Entity::find_by_id(token.to_string())
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    Ok(users::Entity::find_by_id(row.user_id).one(db).await?)
}

// --- Verification, subscriptions, usage, and phone inventory ---

pub async fn owned_number_for_user(
    db: &DatabaseConnection,
    user_id: Uuid,
) -> Result<Option<phone_numbers::Model>> {
    Ok(phone_numbers::Entity::find()
        .filter(phone_numbers::Column::DedicatedUserId.eq(user_id))
        .filter(phone_numbers::Column::Purpose.eq("dedicated"))
        .filter(phone_numbers::Column::Status.eq("active"))
        .one(db)
        .await?)
}

pub async fn phone_number_by_id(
    db: &DatabaseConnection,
    id: Uuid,
) -> Result<Option<phone_numbers::Model>> {
    Ok(phone_numbers::Entity::find_by_id(id).one(db).await?)
}

pub async fn owned_number_by_phone(
    db: &DatabaseConnection,
    phone_number: &str,
) -> Result<Option<phone_numbers::Model>> {
    Ok(phone_numbers::Entity::find()
        .filter(phone_numbers::Column::PhoneNumber.eq(phone_number))
        .filter(phone_numbers::Column::Purpose.eq("dedicated"))
        .filter(phone_numbers::Column::Status.eq("active"))
        .one(db)
        .await?)
}

pub async fn all_active_numbers(db: &DatabaseConnection) -> Result<Vec<phone_numbers::Model>> {
    Ok(phone_numbers::Entity::find()
        .filter(phone_numbers::Column::Status.eq("active"))
        .all(db)
        .await?)
}

pub async fn assign_owned_number(
    db: &DatabaseConnection,
    user_id: Uuid,
    twilio_sid: &str,
    phone_number: &str,
    friendly_name: &str,
) -> Result<phone_numbers::Model> {
    Ok(phone_numbers::ActiveModel {
        id: Set(Uuid::new_v4()),
        twilio_sid: Set(twilio_sid.to_string()),
        phone_number: Set(phone_number.to_string()),
        friendly_name: Set(friendly_name.to_string()),
        country_code: Set("US".into()),
        purpose: Set("dedicated".into()),
        status: Set("active".into()),
        dedicated_user_id: Set(Some(user_id)),
        purchased_at: Set(Utc::now()),
        release_due_at: Set(None),
        last_used_at: Set(None),
        released_at: Set(None),
        last_error: Set(None),
    }
    .insert(db)
    .await?)
}

pub async fn create_pool_number(
    db: &DatabaseConnection,
    twilio_sid: &str,
    phone_number: &str,
    friendly_name: &str,
    release_due_at: DateTime<Utc>,
) -> Result<phone_numbers::Model> {
    Ok(phone_numbers::ActiveModel {
        id: Set(Uuid::new_v4()),
        twilio_sid: Set(twilio_sid.into()),
        phone_number: Set(phone_number.into()),
        friendly_name: Set(friendly_name.into()),
        country_code: Set("US".into()),
        purpose: Set("trial_pool".into()),
        status: Set("active".into()),
        dedicated_user_id: Set(None),
        purchased_at: Set(Utc::now()),
        release_due_at: Set(Some(release_due_at)),
        last_used_at: Set(None),
        released_at: Set(None),
        last_error: Set(None),
    }
    .insert(db)
    .await?)
}

pub async fn available_pool_number(
    db: &DatabaseConnection,
) -> Result<Option<phone_numbers::Model>> {
    Ok(phone_numbers::Entity::find()
        .filter(phone_numbers::Column::Purpose.eq("trial_pool"))
        .filter(phone_numbers::Column::Status.eq("active"))
        .order_by_asc(phone_numbers::Column::LastUsedAt)
        .one(db)
        .await?)
}

pub async fn touch_number(db: &DatabaseConnection, id: Uuid) -> Result<()> {
    let Some(model) = phone_numbers::Entity::find_by_id(id).one(db).await? else {
        return Ok(());
    };
    let mut active: phone_numbers::ActiveModel = model.into();
    active.last_used_at = Set(Some(Utc::now()));
    active.update(db).await?;
    Ok(())
}

pub async fn pool_purchases_since(db: &DatabaseConnection, since: DateTime<Utc>) -> Result<usize> {
    Ok(phone_numbers::Entity::find()
        .filter(phone_numbers::Column::Purpose.eq("trial_pool"))
        .filter(phone_numbers::Column::PurchasedAt.gte(since))
        .all(db)
        .await?
        .len())
}

pub async fn active_pool_count(db: &DatabaseConnection) -> Result<usize> {
    Ok(phone_numbers::Entity::find()
        .filter(phone_numbers::Column::Purpose.eq("trial_pool"))
        .filter(phone_numbers::Column::Status.eq("active"))
        .all(db)
        .await?
        .len())
}

pub async fn releasable_numbers(db: &DatabaseConnection) -> Result<Vec<phone_numbers::Model>> {
    Ok(phone_numbers::Entity::find()
        .filter(phone_numbers::Column::Status.eq("active"))
        .all(db)
        .await?)
}

pub async fn number_has_active_call(db: &DatabaseConnection, id: Uuid) -> Result<bool> {
    Ok(calls::Entity::find()
        .filter(calls::Column::FromNumberId.eq(id))
        .filter(calls::Column::Status.is_in(["queued", "ringing", "in_progress"]))
        .one(db)
        .await?
        .is_some())
}

pub async fn mark_number_released(db: &DatabaseConnection, id: Uuid) -> Result<()> {
    let Some(model) = phone_numbers::Entity::find_by_id(id).one(db).await? else {
        return Ok(());
    };
    let mut active: phone_numbers::ActiveModel = model.into();
    active.status = Set("released".into());
    active.released_at = Set(Some(Utc::now()));
    active.dedicated_user_id = Set(None);
    active.update(db).await?;
    Ok(())
}

pub async fn extend_pool_renewal(db: &DatabaseConnection, id: Uuid) -> Result<()> {
    let Some(model) = phone_numbers::Entity::find_by_id(id).one(db).await? else {
        return Ok(());
    };
    let mut active: phone_numbers::ActiveModel = model.into();
    active.release_due_at = Set(Some(Utc::now() + chrono::Duration::days(30)));
    active.update(db).await?;
    Ok(())
}

pub async fn verified_phone_for_user(
    db: &DatabaseConnection,
    user_id: Uuid,
) -> Result<Option<verified_phones::Model>> {
    Ok(verified_phones::Entity::find()
        .filter(verified_phones::Column::UserId.eq(user_id))
        .one(db)
        .await?)
}

pub async fn verified_phone_by_hash(
    db: &DatabaseConnection,
    phone_hash: &str,
) -> Result<Option<verified_phones::Model>> {
    Ok(verified_phones::Entity::find()
        .filter(verified_phones::Column::PhoneHash.eq(phone_hash))
        .one(db)
        .await?)
}

pub async fn create_attestation_challenge(
    db: &DatabaseConnection,
    user_id: Uuid,
) -> Result<String> {
    let challenge = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    attestation_challenges::ActiveModel {
        challenge: Set(challenge.clone()),
        user_id: Set(user_id),
        expires_at: Set(Utc::now() + chrono::Duration::minutes(5)),
        used: Set(false),
    }
    .insert(db)
    .await?;
    Ok(challenge)
}

pub async fn consume_attestation_challenge(
    db: &DatabaseConnection,
    challenge: &str,
    user_id: Uuid,
) -> Result<bool> {
    let Some(model) = attestation_challenges::Entity::find_by_id(challenge.to_string())
        .one(db)
        .await?
    else {
        return Ok(false);
    };
    if model.user_id != user_id || model.used || model.expires_at <= Utc::now() {
        return Ok(false);
    }
    let mut active: attestation_challenges::ActiveModel = model.into();
    active.used = Set(true);
    active.update(db).await?;
    Ok(true)
}

pub async fn device_attestation_for_user(
    db: &DatabaseConnection,
    user_id: Uuid,
) -> Result<Option<device_attestations::Model>> {
    Ok(device_attestations::Entity::find()
        .filter(device_attestations::Column::UserId.eq(user_id))
        .one(db)
        .await?)
}

pub async fn device_attestation_by_key(
    db: &DatabaseConnection,
    key_id: &str,
) -> Result<Option<device_attestations::Model>> {
    Ok(device_attestations::Entity::find()
        .filter(device_attestations::Column::KeyId.eq(key_id))
        .one(db)
        .await?)
}

pub async fn save_device_attestation(
    db: &DatabaseConnection,
    user_id: Uuid,
    key_id: &str,
    public_key: &str,
    receipt: &str,
    device_token: &str,
    trial_previously_used: bool,
) -> Result<device_attestations::Model> {
    Ok(device_attestations::ActiveModel {
        id: Set(Uuid::new_v4()),
        user_id: Set(user_id),
        key_id: Set(key_id.into()),
        public_key: Set(public_key.into()),
        receipt: Set(receipt.into()),
        device_token: Set(device_token.into()),
        trial_previously_used: Set(trial_previously_used),
        attested_at: Set(Utc::now()),
    }
    .insert(db)
    .await?)
}

pub async fn mark_device_trial_used(db: &DatabaseConnection, user_id: Uuid) -> Result<()> {
    let Some(model) = device_attestation_for_user(db, user_id).await? else {
        return Ok(());
    };
    let mut active: device_attestations::ActiveModel = model.into();
    active.trial_previously_used = Set(true);
    active.update(db).await?;
    Ok(())
}

pub async fn save_verified_phone(
    db: &DatabaseConnection,
    user_id: Uuid,
    phone_hash: &str,
    masked_phone: &str,
    line_type: &str,
    country_code: &str,
    caller_id_number: &str,
    outgoing_caller_id_sid: &str,
) -> Result<verified_phones::Model> {
    if let Some(existing) = verified_phone_for_user(db, user_id).await? {
        let mut active: verified_phones::ActiveModel = existing.into();
        active.phone_hash = Set(phone_hash.into());
        active.masked_phone = Set(masked_phone.into());
        active.line_type = Set(line_type.into());
        active.country_code = Set(country_code.into());
        active.caller_id_number = Set(Some(caller_id_number.into()));
        active.outgoing_caller_id_sid = Set(Some(outgoing_caller_id_sid.into()));
        active.verified_at = Set(Utc::now());
        return Ok(active.update(db).await?);
    }
    Ok(verified_phones::ActiveModel {
        id: Set(Uuid::new_v4()),
        user_id: Set(user_id),
        phone_hash: Set(phone_hash.into()),
        masked_phone: Set(masked_phone.into()),
        line_type: Set(line_type.into()),
        country_code: Set(country_code.into()),
        caller_id_number: Set(Some(caller_id_number.into())),
        outgoing_caller_id_sid: Set(Some(outgoing_caller_id_sid.into())),
        verified_at: Set(Utc::now()),
    }
    .insert(db)
    .await?)
}

pub async fn current_subscription(
    db: &DatabaseConnection,
    user_id: Uuid,
) -> Result<Option<subscriptions::Model>> {
    Ok(subscriptions::Entity::find()
        .filter(subscriptions::Column::UserId.eq(user_id))
        .one(db)
        .await?)
}

pub async fn upsert_subscription(
    db: &DatabaseConnection,
    user_id: Uuid,
    product_id: &str,
    original_transaction_id: &str,
    status: &str,
    period_start: DateTime<Utc>,
    period_end: DateTime<Utc>,
    auto_renew: bool,
    environment: &str,
) -> Result<subscriptions::Model> {
    if let Some(existing) = current_subscription(db, user_id).await? {
        let mut active: subscriptions::ActiveModel = existing.into();
        active.product_id = Set(product_id.into());
        active.original_transaction_id = Set(original_transaction_id.into());
        active.status = Set(status.into());
        active.period_start = Set(period_start);
        active.period_end = Set(period_end);
        active.auto_renew = Set(auto_renew);
        active.environment = Set(environment.into());
        active.updated_at = Set(Utc::now());
        return Ok(active.update(db).await?);
    }
    Ok(subscriptions::ActiveModel {
        id: Set(Uuid::new_v4()),
        user_id: Set(user_id),
        product_id: Set(product_id.into()),
        original_transaction_id: Set(original_transaction_id.into()),
        status: Set(status.into()),
        period_start: Set(period_start),
        period_end: Set(period_end),
        auto_renew: Set(auto_renew),
        environment: Set(environment.into()),
        updated_at: Set(Utc::now()),
    }
    .insert(db)
    .await?)
}

pub async fn usage_seconds(
    db: &DatabaseConnection,
    user_id: Uuid,
    bucket: &str,
    period_start: Option<DateTime<Utc>>,
) -> Result<i64> {
    let mut query = usage_records::Entity::find()
        .filter(usage_records::Column::UserId.eq(user_id))
        .filter(usage_records::Column::Bucket.eq(bucket));
    if let Some(start) = period_start {
        query = query.filter(usage_records::Column::PeriodStart.eq(start));
    }
    Ok(query
        .all(db)
        .await?
        .into_iter()
        .map(|row| row.connected_seconds)
        .sum())
}

pub async fn has_active_call(db: &DatabaseConnection, user_id: Uuid) -> Result<bool> {
    Ok(calls::Entity::find()
        .filter(calls::Column::UserId.eq(user_id))
        .filter(calls::Column::Status.is_in(["queued", "ringing", "in_progress"]))
        .one(db)
        .await?
        .is_some())
}

/// Idempotently records Twilio's final connected duration for one call.
pub async fn finalize_call_usage(
    db: &DatabaseConnection,
    call_id: Uuid,
    connected_seconds: i64,
) -> Result<()> {
    let Some(model) = find_call(db, call_id).await? else {
        return Ok(());
    };
    if model.usage_finalized {
        return Ok(());
    }
    let seconds = connected_seconds
        .max(0)
        .min(model.quota_cap_seconds.unwrap_or(i64::MAX));
    if let (Some(user_id), Some(bucket)) = (model.user_id, model.usage_bucket.clone()) {
        let subscription = current_subscription(db, user_id).await?;
        let (period_start, period_end) = if bucket == "paid" {
            subscription
                .map(|s| (Some(s.period_start), Some(s.period_end)))
                .unwrap_or((None, None))
        } else {
            (None, None)
        };
        let insert = usage_records::ActiveModel {
            id: Set(Uuid::new_v4()),
            user_id: Set(user_id),
            call_id: Set(call_id),
            bucket: Set(bucket),
            period_start: Set(period_start),
            period_end: Set(period_end),
            connected_seconds: Set(seconds),
            finalized_at: Set(Utc::now()),
        }
        .insert(db)
        .await;
        if let Err(err) = insert {
            // A duplicate call_id means a concurrent callback already finalized it.
            if usage_records::Entity::find()
                .filter(usage_records::Column::CallId.eq(call_id))
                .one(db)
                .await?
                .is_none()
            {
                return Err(err.into());
            }
        }
    }
    let mut active: calls::ActiveModel = model.into();
    active.connected_seconds = Set(seconds);
    active.usage_finalized = Set(true);
    active.ended_at = Set(Some(Utc::now()));
    active.update(db).await?;
    Ok(())
}

pub async fn account_status(
    db: &DatabaseConnection,
    user_id: Uuid,
) -> Result<AccountStatusResponse> {
    let phone = verified_phone_for_user(db, user_id).await?;
    let subscription = current_subscription(db, user_id).await?;
    let now = Utc::now();
    let paid = subscription.as_ref().is_some_and(|s| {
        matches!(s.status.as_str(), "active" | "grace_period") && s.period_end > now
    });
    let trial_used = usage_seconds(db, user_id, "trial", None).await?;
    let paid_used = match subscription.as_ref().filter(|_| paid) {
        Some(s) => usage_seconds(db, user_id, "paid", Some(s.period_start)).await?,
        None => 0,
    };
    let dedicated = owned_number_for_user(db, user_id)
        .await?
        .map(|number| OwnedNumber {
            phone_number: number.phone_number,
            friendly_name: number.friendly_name,
            inbound_enabled: true,
        });
    let entitlement = if paid {
        if subscription
            .as_ref()
            .is_some_and(|s| s.status == "grace_period")
        {
            EntitlementStatus::GracePeriod
        } else {
            EntitlementStatus::Active
        }
    } else if trial_used < TRIAL_SECONDS_LIMIT {
        EntitlementStatus::FreeTrial
    } else {
        EntitlementStatus::Expired
    };
    let phone_verified = phone.as_ref().is_some_and(|phone| {
        phone.caller_id_number.is_some() && phone.outgoing_caller_id_sid.is_some()
    });
    let verified_phone_masked = phone
        .as_ref()
        .filter(|_| phone_verified)
        .map(|phone| phone.masked_phone.clone());
    Ok(AccountStatusResponse {
        phone_verified,
        device_attested: device_attestation_for_user(db, user_id).await?.is_some(),
        app_attest_required: false,
        verified_phone_masked,
        tier: if paid {
            AccountTier::Paid
        } else {
            AccountTier::Free
        },
        entitlement,
        trial_seconds_limit: TRIAL_SECONDS_LIMIT,
        trial_seconds_used: trial_used,
        included_seconds_limit: PAID_SECONDS_LIMIT,
        included_seconds_used: paid_used,
        period_end: subscription.filter(|_| paid).map(|s| s.period_end),
        // Outbound calls use the user's verified mobile number. A dedicated
        // Courtside number remains optional for future inbound-call support.
        needs_number_claim: false,
        dedicated_number: dedicated,
    })
}

// --- Chat ---

const CHAT_UPDATE_ROLE: &str = "assistant_update";

fn chat_to_shared(model: &chat_messages::Model) -> ChatMessage {
    ChatMessage {
        id: model.id,
        role: if model.role == CHAT_UPDATE_ROLE {
            ChatRole::Assistant
        } else {
            from_str(&model.role)
        },
        content: model.content.clone(),
        at: model.at,
    }
}

fn chat_to_summary(model: &chat_sessions::Model) -> ChatSummary {
    ChatSummary {
        id: model.id,
        title: model.title.clone(),
        updated_at: model.updated_at,
    }
}

pub async fn create_chat_session(
    db: &DatabaseConnection,
    user_id: Uuid,
) -> Result<chat_sessions::Model> {
    let now = Utc::now();
    Ok(chat_sessions::ActiveModel {
        id: Set(Uuid::new_v4()),
        user_id: Set(user_id),
        title: Set("New chat".into()),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(db)
    .await?)
}

pub async fn ensure_current_chat(
    db: &DatabaseConnection,
    user_id: Uuid,
) -> Result<chat_sessions::Model> {
    let user = find_user_by_id(db, user_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("user not found"))?;
    if let Some(chat_id) = user.current_chat_id {
        if let Some(chat) = chat_sessions::Entity::find_by_id(chat_id)
            .filter(chat_sessions::Column::UserId.eq(user_id))
            .one(db)
            .await?
        {
            return Ok(chat);
        }
    }

    let chat = create_chat_session(db, user_id).await?;
    let mut active: users::ActiveModel = user.into();
    active.current_chat_id = Set(Some(chat.id));
    active.update(db).await?;
    Ok(chat)
}

/// Incoming calls have their own conversation without changing the chat the
/// owner is composing in. Viewing the call explicitly selects this chat.
pub async fn create_incoming_call_chat(
    db: &DatabaseConnection,
    user_id: Uuid,
    peer: &str,
) -> Result<Uuid> {
    let chat = create_chat_session(db, user_id).await?;
    touch_chat(db, chat.id, Some(&format!("Incoming call from {peer}"))).await?;
    Ok(chat.id)
}

pub async fn recent_chats(db: &DatabaseConnection, user_id: Uuid) -> Result<Vec<ChatSummary>> {
    ensure_current_chat(db, user_id).await?;
    let chats = chat_sessions::Entity::find()
        .filter(chat_sessions::Column::UserId.eq(user_id))
        .order_by_desc(chat_sessions::Column::UpdatedAt)
        .all(db)
        .await?;
    Ok(chats.iter().map(chat_to_summary).collect())
}

pub async fn select_chat(
    db: &DatabaseConnection,
    user_id: Uuid,
    chat_id: Uuid,
) -> Result<Option<chat_sessions::Model>> {
    let Some(chat) = chat_sessions::Entity::find_by_id(chat_id)
        .filter(chat_sessions::Column::UserId.eq(user_id))
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let Some(user) = find_user_by_id(db, user_id).await? else {
        return Ok(None);
    };
    let mut active: users::ActiveModel = user.into();
    active.current_chat_id = Set(Some(chat.id));
    active.update(db).await?;
    Ok(Some(chat))
}

pub async fn chat_history_for(
    db: &DatabaseConnection,
    user_id: Uuid,
    chat_id: Uuid,
) -> Result<Vec<ChatMessage>> {
    let models = chat_messages::Entity::find()
        .filter(chat_messages::Column::UserId.eq(user_id))
        .filter(chat_messages::Column::ChatId.eq(chat_id))
        .order_by_asc(chat_messages::Column::At)
        .all(db)
        .await?;

    // Chat replies are saved as atomic user/assistant exchanges. Older builds
    // could leave a user row behind when the model request failed; pair each
    // assistant response with the most recent user row so those stale drafts
    // never appear in history or get sent back to the model as context.
    let mut history = Vec::with_capacity(models.len());
    let mut pending_user = None;
    for model in &models {
        // Call results originate on the server and intentionally do not have a
        // paired user message. They must remain visible while legacy orphaned
        // user drafts continue to be filtered out.
        if model.role == CHAT_UPDATE_ROLE {
            pending_user = None;
            history.push(chat_to_shared(model));
            continue;
        }
        let message = chat_to_shared(model);
        match message.role {
            ChatRole::User => pending_user = Some(message),
            ChatRole::Assistant => {
                if let Some(user_message) = pending_user.take() {
                    history.push(user_message);
                    history.push(message);
                }
            }
        }
    }
    Ok(history)
}

pub async fn chat_history(db: &DatabaseConnection, user_id: Uuid) -> Result<Vec<ChatMessage>> {
    let chat = ensure_current_chat(db, user_id).await?;
    chat_history_for(db, user_id, chat.id).await
}

pub async fn latest_call_in_chat(
    db: &DatabaseConnection,
    user_id: Uuid,
    chat_id: Uuid,
) -> Result<Option<calls::Model>> {
    Ok(calls::Entity::find()
        .filter(calls::Column::UserId.eq(user_id))
        .filter(calls::Column::ChatId.eq(chat_id))
        .order_by_desc(calls::Column::CreatedAt)
        .one(db)
        .await?)
}

pub async fn latest_call_in_current_chat(
    db: &DatabaseConnection,
    user_id: Uuid,
) -> Result<Option<calls::Model>> {
    let chat = ensure_current_chat(db, user_id).await?;
    latest_call_in_chat(db, user_id, chat.id).await
}

pub async fn start_new_chat(
    db: &DatabaseConnection,
    user_id: Uuid,
) -> Result<chat_sessions::Model> {
    let current = ensure_current_chat(db, user_id).await?;
    let message_count = chat_messages::Entity::find()
        .filter(chat_messages::Column::UserId.eq(user_id))
        .filter(chat_messages::Column::ChatId.eq(current.id))
        .count(db)
        .await?;
    let call_count = calls::Entity::find()
        .filter(calls::Column::UserId.eq(user_id))
        .filter(calls::Column::ChatId.eq(current.id))
        .count(db)
        .await?;
    if message_count == 0 && call_count == 0 {
        return Ok(current);
    }

    let chat = create_chat_session(db, user_id).await?;
    let user = find_user_by_id(db, user_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("user not found"))?;
    let mut active: users::ActiveModel = user.into();
    active.current_chat_id = Set(Some(chat.id));
    active.update(db).await?;
    Ok(chat)
}

fn chat_title(content: &str) -> String {
    let compact = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = compact.chars();
    let title = chars.by_ref().take(48).collect::<String>();
    if chars.next().is_some() {
        format!("{}…", title.trim_end())
    } else if title.is_empty() {
        "New chat".into()
    } else {
        title
    }
}

async fn touch_chat(
    db: &DatabaseConnection,
    chat_id: Uuid,
    first_user_message: Option<&str>,
) -> Result<()> {
    let Some(model) = chat_sessions::Entity::find_by_id(chat_id).one(db).await? else {
        return Ok(());
    };
    let mut active: chat_sessions::ActiveModel = model.clone().into();
    if model.title == "New chat" {
        if let Some(content) = first_user_message {
            active.title = Set(chat_title(content));
        }
    }
    active.updated_at = Set(Utc::now());
    active.update(db).await?;
    Ok(())
}

pub async fn set_call_chat_id(db: &DatabaseConnection, call_id: Uuid, chat_id: Uuid) -> Result<()> {
    let Some(model) = find_call(db, call_id).await? else {
        return Ok(());
    };
    let mut active: calls::ActiveModel = model.into();
    active.chat_id = Set(Some(chat_id));
    active.update(db).await?;
    touch_chat(db, chat_id, None).await
}

pub async fn add_chat_exchange(
    db: &DatabaseConnection,
    user_id: Uuid,
    chat_id: Uuid,
    user_content: &str,
    assistant_content: &str,
) -> Result<(ChatMessage, ChatMessage)> {
    let transaction = db.begin().await?;
    let user_at = Utc::now();
    let user_model = chat_messages::ActiveModel {
        id: Set(Uuid::new_v4()),
        user_id: Set(user_id),
        chat_id: Set(Some(chat_id)),
        role: Set(to_str(&ChatRole::User)),
        content: Set(user_content.to_string()),
        at: Set(user_at),
    }
    .insert(&transaction)
    .await?;
    let assistant_model = chat_messages::ActiveModel {
        id: Set(Uuid::new_v4()),
        user_id: Set(user_id),
        chat_id: Set(Some(chat_id)),
        role: Set(to_str(&ChatRole::Assistant)),
        content: Set(assistant_content.to_string()),
        at: Set(user_at + chrono::Duration::microseconds(1)),
    }
    .insert(&transaction)
    .await?;
    transaction.commit().await?;
    touch_chat(db, chat_id, Some(user_content)).await?;
    Ok((
        chat_to_shared(&user_model),
        chat_to_shared(&assistant_model),
    ))
}

/// Persist a proactive assistant update that was not triggered by a new chat
/// message, such as a completed-call result.
pub async fn add_assistant_chat_update(
    db: &DatabaseConnection,
    user_id: Uuid,
    chat_id: Uuid,
    content: &str,
) -> Result<ChatMessage> {
    let model = chat_messages::ActiveModel {
        id: Set(Uuid::new_v4()),
        user_id: Set(user_id),
        chat_id: Set(Some(chat_id)),
        role: Set(CHAT_UPDATE_ROLE.into()),
        content: Set(content.to_string()),
        at: Set(Utc::now()),
    }
    .insert(db)
    .await?;
    touch_chat(db, chat_id, None).await?;
    Ok(chat_to_shared(&model))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::PaginatorTrait;

    #[tokio::test]
    async fn chat_history_hides_legacy_unanswered_messages() {
        let db = connect("sqlite::memory:").await.unwrap();
        let user = create_user(&db, "chat@example.com", "Chat", "unused")
            .await
            .unwrap();
        let chat = ensure_current_chat(&db, user.id).await.unwrap();

        chat_messages::ActiveModel {
            id: Set(Uuid::new_v4()),
            user_id: Set(user.id),
            chat_id: Set(Some(chat.id)),
            role: Set(to_str(&ChatRole::User)),
            content: Set("This request failed before it got a reply".into()),
            at: Set(Utc::now() - chrono::Duration::seconds(1)),
        }
        .insert(&db)
        .await
        .unwrap();

        add_chat_exchange(
            &db,
            user.id,
            chat.id,
            "Plan a clinic call",
            "What is the clinic's name?",
        )
        .await
        .unwrap();

        let history = chat_history(&db, user.id).await.unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].content, "Plan a clinic call");
        assert_eq!(history[1].content, "What is the clinic's name?");
    }

    #[tokio::test]
    async fn new_chat_starts_fresh_context_without_deleting_old_rows() {
        let db = connect("sqlite::memory:").await.unwrap();
        let user = create_user(&db, "fresh-chat@example.com", "Fresh", "unused")
            .await
            .unwrap();
        let first_chat = ensure_current_chat(&db, user.id).await.unwrap();

        add_chat_exchange(&db, user.id, first_chat.id, "Old question", "Old answer")
            .await
            .unwrap();
        let second_chat = start_new_chat(&db, user.id).await.unwrap();
        assert!(chat_history(&db, user.id).await.unwrap().is_empty());

        add_chat_exchange(&db, user.id, second_chat.id, "New question", "New answer")
            .await
            .unwrap();
        let history = chat_history(&db, user.id).await.unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].content, "New question");

        let stored_count = chat_messages::Entity::find()
            .filter(chat_messages::Column::UserId.eq(user.id))
            .count(&db)
            .await
            .unwrap();
        assert_eq!(stored_count, 4);

        let chats = recent_chats(&db, user.id).await.unwrap();
        assert_eq!(chats.len(), 2);
        assert_eq!(chats[0].title, "New question");

        select_chat(&db, user.id, first_chat.id)
            .await
            .unwrap()
            .unwrap();
        let restored = chat_history(&db, user.id).await.unwrap();
        assert_eq!(restored.len(), 2);
        assert_eq!(restored[0].content, "Old question");
    }

    #[tokio::test]
    async fn new_chat_reuses_an_existing_empty_session() {
        let db = connect("sqlite::memory:").await.unwrap();
        let user = create_user(&db, "empty-chat@example.com", "Empty", "unused")
            .await
            .unwrap();
        let first_chat = ensure_current_chat(&db, user.id).await.unwrap();

        let reopened_chat = start_new_chat(&db, user.id).await.unwrap();

        assert_eq!(reopened_chat.id, first_chat.id);
        assert_eq!(recent_chats(&db, user.id).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn calls_stay_attached_to_the_chat_that_started_them() {
        let db = connect("sqlite::memory:").await.unwrap();
        let user = create_user(&db, "call-boundary@example.com", "Boundary", "unused")
            .await
            .unwrap();
        let first_chat = ensure_current_chat(&db, user.id).await.unwrap();
        let call = create_call(
            &db,
            user.id,
            CallDirection::Outbound,
            "+14155550100",
            Some("Test"),
        )
        .await
        .unwrap();
        set_call_chat_id(&db, call.id, first_chat.id).await.unwrap();

        let second_chat = start_new_chat(&db, user.id).await.unwrap();
        assert!(latest_call_in_chat(&db, user.id, second_chat.id)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            latest_call_in_chat(&db, user.id, first_chat.id)
                .await
                .unwrap()
                .unwrap()
                .id,
            call.id
        );
    }

    #[tokio::test]
    async fn call_preferences_default_and_persist() {
        let db = connect("sqlite::memory:").await.unwrap();
        let user = create_user(&db, "preferences@example.com", "Jaspreet", "unused")
            .await
            .unwrap();
        let defaults = call_preferences(&user);
        assert_eq!(defaults.voice, CallVoice::Marin);
        assert!(defaults.announce_courtside);
        assert!(defaults.custom_instructions.is_empty());

        let updated = update_call_preferences(
            &db,
            user,
            CallVoice::Cedar,
            false,
            Some("Keep the call under two minutes."),
            Some("+15305550123"),
        )
        .await
        .unwrap();
        assert_eq!(updated.voice, CallVoice::Cedar);
        assert!(!updated.announce_courtside);
        assert_eq!(
            updated.custom_instructions,
            "Keep the call under two minutes."
        );
        assert_eq!(updated.takeover_phone_number, "+15305550123");
    }

    #[tokio::test]
    async fn proactive_assistant_updates_are_visible_without_a_user_pair() {
        let db = connect("sqlite::memory:").await.unwrap();
        let user = create_user(&db, "updates@example.com", "Updates", "unused")
            .await
            .unwrap();
        let chat = ensure_current_chat(&db, user.id).await.unwrap();

        let update = add_assistant_chat_update(
            &db,
            user.id,
            chat.id,
            "Call update for +14155551212: the reservation is confirmed.",
        )
        .await
        .unwrap();

        let history = chat_history(&db, user.id).await.unwrap();
        assert_eq!(history, vec![update]);
        assert_eq!(history[0].role, ChatRole::Assistant);
    }

    #[tokio::test]
    async fn lifetime_trial_and_paid_period_usage_are_server_authoritative() {
        let db = connect("sqlite::memory:").await.unwrap();
        let user = create_user(&db, "quota@example.com", "Quota", "unused")
            .await
            .unwrap();
        save_verified_phone(
            &db,
            user.id,
            "phone-hash",
            "•••-•••-1212",
            "mobile",
            "US",
            "+14155551212",
            "PN123",
        )
        .await
        .unwrap();

        let trial = create_metered_call(
            &db,
            user.id,
            CallDirection::Outbound,
            "+14155551212",
            Some("test"),
            None,
            Some("trial"),
            Some(TRIAL_SECONDS_LIMIT),
            true,
            false,
            CallAiBackend::OpenAiLive,
        )
        .await
        .unwrap();
        assert!(
            !find_call(&db, trial.id)
                .await
                .unwrap()
                .unwrap()
                .share_user_name
        );
        finalize_call_usage(&db, trial.id, TRIAL_SECONDS_LIMIT + 50)
            .await
            .unwrap();
        finalize_call_usage(&db, trial.id, 1).await.unwrap();
        let free = account_status(&db, user.id).await.unwrap();
        assert_eq!(free.tier, AccountTier::Free);
        assert_eq!(free.trial_seconds_used, TRIAL_SECONDS_LIMIT);
        assert_eq!(free.entitlement, EntitlementStatus::Expired);

        let start = Utc::now() - chrono::Duration::minutes(1);
        let end = Utc::now() + chrono::Duration::days(30);
        upsert_subscription(
            &db,
            user.id,
            "com.malakindustries.courtside.pro.monthly",
            "original-transaction",
            "active",
            start,
            end,
            true,
            "sandbox",
        )
        .await
        .unwrap();
        let paid_call = create_metered_call(
            &db,
            user.id,
            CallDirection::Inbound,
            "+14155559876",
            None,
            None,
            Some("paid"),
            Some(PAID_SECONDS_LIMIT),
            true,
            true,
            CallAiBackend::OpenAiLive,
        )
        .await
        .unwrap();
        finalize_call_usage(&db, paid_call.id, 120).await.unwrap();
        let paid = account_status(&db, user.id).await.unwrap();
        assert_eq!(paid.tier, AccountTier::Paid);
        assert_eq!(paid.included_seconds_used, 120);
        assert_eq!(paid.trial_seconds_used, TRIAL_SECONDS_LIMIT);
    }
}
