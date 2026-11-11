use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "phone_numbers")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    #[sea_orm(unique)]
    pub twilio_sid: String,
    #[sea_orm(unique)]
    pub phone_number: String,
    pub friendly_name: String,
    pub country_code: String,
    /// `trial_pool` or `dedicated`.
    pub purpose: String,
    /// `active`, `draining`, `release_pending`, `released`, or `error`.
    pub status: String,
    #[sea_orm(unique)]
    pub dedicated_user_id: Option<Uuid>,
    pub purchased_at: DateTimeUtc,
    pub release_due_at: Option<DateTimeUtc>,
    pub last_used_at: Option<DateTimeUtc>,
    pub released_at: Option<DateTimeUtc>,
    pub last_error: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
