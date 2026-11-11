use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "transcript_segments")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub call_id: Uuid,
    pub speaker: String,
    pub text: String,
    pub at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::calls::Entity",
        from = "Column::CallId",
        to = "super::calls::Column::Id"
    )]
    Call,
}

impl Related<super::calls::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Call.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
