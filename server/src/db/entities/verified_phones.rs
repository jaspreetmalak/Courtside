use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "verified_phones")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    #[sea_orm(unique)]
    pub user_id: Uuid,
    #[sea_orm(unique)]
    pub phone_hash: String,
    pub masked_phone: String,
    pub line_type: String,
    pub country_code: String,
    /// E.164 number Twilio approved for outbound Caller ID use.
    pub caller_id_number: Option<String>,
    /// Twilio OutgoingCallerId SID (PN...).
    pub outgoing_caller_id_sid: Option<String>,
    pub verified_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
