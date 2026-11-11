use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "calls")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub user_id: Option<Uuid>,
    pub chat_id: Option<Uuid>,
    pub direction: String,
    pub peer_number: String,
    pub task_prompt: Option<String>,
    pub ai_backend: String,
    pub status: String,
    pub twilio_call_sid: Option<String>,
    pub started_at: Option<DateTimeUtc>,
    pub ended_at: Option<DateTimeUtc>,
    pub summary: Option<String>,
    pub from_number_id: Option<Uuid>,
    pub usage_bucket: Option<String>,
    pub quota_cap_seconds: Option<i64>,
    pub connected_seconds: i64,
    pub usage_finalized: bool,
    pub announce_courtside: bool,
    pub share_user_name: bool,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::transcript_segments::Entity")]
    TranscriptSegments,
}

impl Related<super::transcript_segments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::TranscriptSegments.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
