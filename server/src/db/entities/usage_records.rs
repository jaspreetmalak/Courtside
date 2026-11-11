use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "usage_records")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub user_id: Uuid,
    #[sea_orm(unique)]
    pub call_id: Uuid,
    /// `trial` or `paid`.
    pub bucket: String,
    pub period_start: Option<DateTimeUtc>,
    pub period_end: Option<DateTimeUtc>,
    pub connected_seconds: i64,
    pub finalized_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
