use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "attestation_challenges")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub challenge: String,
    pub user_id: Uuid,
    pub expires_at: DateTimeUtc,
    pub used: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
