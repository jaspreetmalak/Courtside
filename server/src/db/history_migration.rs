use sea_orm_migration::prelude::*;

pub struct AddHistoryLookup;

impl MigrationName for AddHistoryLookup {
    fn name(&self) -> &str {
        "m20261011_000013_chat_history_lookup"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddHistoryLookup {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("memory_settings"))
                    .add_column(
                        ColumnDef::new(Alias::new("history_enabled"))
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .to_owned(),
            )
            .await?;
        let db = manager.get_connection();
        // External content avoids storing another copy of private messages.
        // Triggers also remove indexed text when the original message is deleted.
        for statement in [
            "CREATE VIRTUAL TABLE chat_history_fts USING fts5(content, content='chat_messages', content_rowid='rowid', tokenize='unicode61 remove_diacritics 2')",
            "CREATE TRIGGER chat_history_insert AFTER INSERT ON chat_messages BEGIN INSERT INTO chat_history_fts(rowid, content) VALUES (new.rowid, new.content); END",
            "CREATE TRIGGER chat_history_delete AFTER DELETE ON chat_messages BEGIN INSERT INTO chat_history_fts(chat_history_fts, rowid, content) VALUES ('delete', old.rowid, old.content); END",
            "CREATE TRIGGER chat_history_update AFTER UPDATE OF content ON chat_messages BEGIN INSERT INTO chat_history_fts(chat_history_fts, rowid, content) VALUES ('delete', old.rowid, old.content); INSERT INTO chat_history_fts(rowid, content) VALUES (new.rowid, new.content); END",
            "INSERT INTO chat_history_fts(chat_history_fts) VALUES ('rebuild')",
            "CREATE INDEX idx_chat_messages_owner_chat_at ON chat_messages(user_id, chat_id, at)",
        ] {
            db.execute_unprepared(statement).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for statement in [
            "DROP TRIGGER chat_history_insert",
            "DROP TRIGGER chat_history_delete",
            "DROP TRIGGER chat_history_update",
            "DROP TABLE chat_history_fts",
            "DROP INDEX idx_chat_messages_owner_chat_at",
        ] {
            manager
                .get_connection()
                .execute_unprepared(statement)
                .await?;
        }
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("memory_settings"))
                    .drop_column(Alias::new("history_enabled"))
                    .to_owned(),
            )
            .await
    }
}
