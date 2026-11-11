//! A single opt-out for lookup through the owner's original chat messages.
use super::entities::chat_history_settings::{self, Column, Entity};
use anyhow::Result;
use sea_orm::{sea_query::OnConflict, ActiveValue::Set, DatabaseConnection, EntityTrait};
use shared::HistorySettingsResponse;
use uuid::Uuid;

pub async fn get(db: &DatabaseConnection, user_id: Uuid) -> Result<HistorySettingsResponse> {
    Ok(HistorySettingsResponse {
        enabled: Entity::find_by_id(user_id)
            .one(db)
            .await?
            .is_none_or(|row| row.enabled),
    })
}

pub async fn enabled(db: &DatabaseConnection, user_id: Uuid) -> Result<bool> {
    Ok(get(db, user_id).await?.enabled)
}

pub async fn set_enabled(
    db: &DatabaseConnection,
    user_id: Uuid,
    enabled: bool,
) -> Result<HistorySettingsResponse> {
    Entity::insert(chat_history_settings::ActiveModel {
        user_id: Set(user_id),
        enabled: Set(enabled),
    })
    .on_conflict(
        OnConflict::column(Column::UserId)
            .update_column(Column::Enabled)
            .to_owned(),
    )
    .exec_without_returning(db)
    .await?;
    get(db, user_id).await
}
