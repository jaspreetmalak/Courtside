use sea_orm_migration::prelude::*;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(CreateTables),
            Box::new(CreateAuthChatTables),
            Box::new(CreateNumberOwnership),
            Box::new(CreateEntitlementsAndInventory),
            Box::new(AddCallPreferences),
            Box::new(AddTakeoverPhone),
            Box::new(CreateChatSessions),
            Box::new(AddCallNamePolicy),
            Box::new(AddCallAiBackend),
            Box::new(AddVoicePreference),
            Box::new(AddVerifiedCallerId),
            Box::new(CreateAccountMemory),
            Box::new(super::history_migration::AddHistoryLookup),
            Box::new(RemoveSavedMemory),
        ]
    }
}

struct CreateAccountMemory;

impl MigrationName for CreateAccountMemory {
    fn name(&self) -> &str {
        "m20261010_000012_account_memory"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CreateAccountMemory {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(MemorySettings::Table)
                    .col(
                        ColumnDef::new(MemorySettings::UserId)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(MemorySettings::Enabled)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(MemorySettings::Revision)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(MemorySettings::Table, MemorySettings::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(SavedMemories::Table)
                    .col(
                        ColumnDef::new(SavedMemories::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(SavedMemories::UserId).uuid().not_null())
                    .col(ColumnDef::new(SavedMemories::Key).string().not_null())
                    .col(ColumnDef::new(SavedMemories::Content).text().not_null())
                    .col(ColumnDef::new(SavedMemories::SourceChatId).uuid())
                    .col(
                        ColumnDef::new(SavedMemories::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SavedMemories::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(SavedMemories::Table, SavedMemories::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(SavedMemories::Table, SavedMemories::SourceChatId)
                            .to(ChatSessions::Table, ChatSessions::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("saved_memories_user_key")
                    .table(SavedMemories::Table)
                    .col(SavedMemories::UserId)
                    .col(SavedMemories::Key)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(SavedMemories::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(MemorySettings::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum MemorySettings {
    Table,
    UserId,
    Enabled,
    Revision,
}

#[derive(DeriveIden)]
enum SavedMemories {
    Table,
    Id,
    UserId,
    Key,
    Content,
    SourceChatId,
    CreatedAt,
    UpdatedAt,
}

struct AddVerifiedCallerId;

impl MigrationName for AddVerifiedCallerId {
    fn name(&self) -> &str {
        "m20260916_000011_verified_caller_id"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddVerifiedCallerId {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(VerifiedPhones::Table)
                    .add_column(ColumnDef::new(VerifiedPhones::CallerIdNumber).string())
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(VerifiedPhones::Table)
                    .add_column(ColumnDef::new(VerifiedPhones::OutgoingCallerIdSid).string())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(VerifiedPhones::Table)
                    .drop_column(VerifiedPhones::OutgoingCallerIdSid)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(VerifiedPhones::Table)
                    .drop_column(VerifiedPhones::CallerIdNumber)
                    .to_owned(),
            )
            .await
    }
}

struct AddVoicePreference;

impl MigrationName for AddVoicePreference {
    fn name(&self) -> &str {
        "m20260915_000010_voice_preference"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddVoicePreference {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .add_column(
                        ColumnDef::new(Users::VoicePreference)
                            .string()
                            .not_null()
                            .default("marin"),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .drop_column(Users::VoicePreference)
                    .to_owned(),
            )
            .await
    }
}

struct AddCallAiBackend;

impl MigrationName for AddCallAiBackend {
    fn name(&self) -> &str {
        "m20260915_000009_call_ai_backend"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddCallAiBackend {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Calls::Table)
                    .add_column(
                        ColumnDef::new(Calls::AiBackend)
                            .string()
                            .not_null()
                            .default("open_ai_live"),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Calls::Table)
                    .drop_column(Calls::AiBackend)
                    .to_owned(),
            )
            .await
    }
}

struct AddCallNamePolicy;

impl MigrationName for AddCallNamePolicy {
    fn name(&self) -> &str {
        "m20260915_000008_call_name_policy"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddCallNamePolicy {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Calls::Table)
                    .add_column(
                        ColumnDef::new(Calls::ShareUserName)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Calls::Table)
                    .drop_column(Calls::ShareUserName)
                    .to_owned(),
            )
            .await
    }
}

struct CreateChatSessions;

impl MigrationName for CreateChatSessions {
    fn name(&self) -> &str {
        "m20260915_000007_chat_sessions"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CreateChatSessions {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(ChatSessions::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ChatSessions::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(ChatSessions::UserId).uuid().not_null())
                    .col(ColumnDef::new(ChatSessions::Title).string().not_null())
                    .col(
                        ColumnDef::new(ChatSessions::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ChatSessions::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(ChatSessions::Table, ChatSessions::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .add_column(ColumnDef::new(Users::CurrentChatId).uuid())
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ChatMessages::Table)
                    .add_column(ColumnDef::new(ChatMessages::ChatId).uuid())
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Calls::Table)
                    .add_column(ColumnDef::new(Calls::ChatId).uuid())
                    .to_owned(),
            )
            .await?;

        // Preserve the currently visible conversation. Older boundary-delimited
        // rows stay unassigned rather than being merged into the new current chat.
        let connection = manager.get_connection();
        connection
            .execute_unprepared(
                "INSERT INTO chat_sessions (id, user_id, title, created_at, updated_at) \
                 SELECT randomblob(16), id, 'New chat', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP FROM users",
            )
            .await?;
        connection
            .execute_unprepared(
                "UPDATE users SET current_chat_id = \
                 (SELECT id FROM chat_sessions WHERE user_id = users.id LIMIT 1)",
            )
            .await?;
        connection
            .execute_unprepared(
                "UPDATE chat_messages SET chat_id = \
                 (SELECT current_chat_id FROM users WHERE users.id = chat_messages.user_id) \
                 WHERE at > COALESCE((SELECT MAX(boundary.at) FROM chat_messages AS boundary \
                   WHERE boundary.user_id = chat_messages.user_id AND boundary.role = 'chat_boundary'), '0001-01-01')",
            )
            .await?;
        connection
            .execute_unprepared(
                "UPDATE calls SET chat_id = \
                 (SELECT current_chat_id FROM users WHERE users.id = calls.user_id) \
                 WHERE created_at > COALESCE((SELECT MAX(boundary.at) FROM chat_messages AS boundary \
                   WHERE boundary.user_id = calls.user_id AND boundary.role = 'chat_boundary'), '0001-01-01')",
            )
            .await?;
        connection
            .execute_unprepared(
                "UPDATE chat_sessions SET title = COALESCE((SELECT substr(content, 1, 48) FROM chat_messages \
                   WHERE chat_id = chat_sessions.id AND role = 'user' ORDER BY at ASC LIMIT 1), 'New chat')",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Calls::Table)
                    .drop_column(Calls::ChatId)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ChatMessages::Table)
                    .drop_column(ChatMessages::ChatId)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .drop_column(Users::CurrentChatId)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(ChatSessions::Table).to_owned())
            .await
    }
}

struct AddTakeoverPhone;

impl MigrationName for AddTakeoverPhone {
    fn name(&self) -> &str {
        "m20260915_000006_takeover_phone"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddTakeoverPhone {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .add_column(ColumnDef::new(Users::TakeoverPhoneNumber).string())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .drop_column(Users::TakeoverPhoneNumber)
                    .to_owned(),
            )
            .await
    }
}

struct AddCallPreferences;

impl MigrationName for AddCallPreferences {
    fn name(&self) -> &str {
        "m20260915_000005_call_preferences"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddCallPreferences {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .add_column(
                        ColumnDef::new(Users::AnnounceCourtside)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .add_column(ColumnDef::new(Users::CustomCallInstructions).text())
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Calls::Table)
                    .add_column(
                        ColumnDef::new(Calls::AnnounceCourtside)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Calls::Table)
                    .drop_column(Calls::AnnounceCourtside)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .drop_column(Users::CustomCallInstructions)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .drop_column(Users::AnnounceCourtside)
                    .to_owned(),
            )
            .await
    }
}

struct CreateEntitlementsAndInventory;

impl MigrationName for CreateEntitlementsAndInventory {
    fn name(&self) -> &str {
        "m20260914_000004_entitlements_inventory"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CreateEntitlementsAndInventory {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Calls::Table)
                    .add_column(ColumnDef::new(Calls::FromNumberId).uuid())
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Calls::Table)
                    .add_column(ColumnDef::new(Calls::UsageBucket).string())
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Calls::Table)
                    .add_column(ColumnDef::new(Calls::QuotaCapSeconds).big_integer())
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Calls::Table)
                    .add_column(
                        ColumnDef::new(Calls::ConnectedSeconds)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Calls::Table)
                    .add_column(
                        ColumnDef::new(Calls::UsageFinalized)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(PhoneNumbers::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(PhoneNumbers::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(PhoneNumbers::TwilioSid)
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(PhoneNumbers::PhoneNumber)
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(PhoneNumbers::FriendlyName)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PhoneNumbers::CountryCode)
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(PhoneNumbers::Purpose).string().not_null())
                    .col(ColumnDef::new(PhoneNumbers::Status).string().not_null())
                    .col(
                        ColumnDef::new(PhoneNumbers::DedicatedUserId)
                            .uuid()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(PhoneNumbers::PurchasedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(PhoneNumbers::ReleaseDueAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(PhoneNumbers::LastUsedAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(PhoneNumbers::ReleasedAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(PhoneNumbers::LastError).text())
                    .foreign_key(
                        ForeignKey::create()
                            .from(PhoneNumbers::Table, PhoneNumbers::DedicatedUserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(AttestationChallenges::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AttestationChallenges::Challenge)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AttestationChallenges::UserId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AttestationChallenges::ExpiresAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AttestationChallenges::Used)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(AttestationChallenges::Table, AttestationChallenges::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(DeviceAttestations::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(DeviceAttestations::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(DeviceAttestations::UserId)
                            .uuid()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(DeviceAttestations::KeyId)
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(DeviceAttestations::PublicKey)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DeviceAttestations::Receipt)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DeviceAttestations::DeviceToken)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DeviceAttestations::TrialPreviouslyUsed)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(DeviceAttestations::AttestedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(DeviceAttestations::Table, DeviceAttestations::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(VerifiedPhones::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(VerifiedPhones::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(VerifiedPhones::UserId)
                            .uuid()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(VerifiedPhones::PhoneHash)
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(VerifiedPhones::MaskedPhone)
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(VerifiedPhones::LineType).string().not_null())
                    .col(
                        ColumnDef::new(VerifiedPhones::CountryCode)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(VerifiedPhones::VerifiedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(VerifiedPhones::Table, VerifiedPhones::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(Subscriptions::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Subscriptions::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Subscriptions::UserId)
                            .uuid()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(Subscriptions::ProductId).string().not_null())
                    .col(
                        ColumnDef::new(Subscriptions::OriginalTransactionId)
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(Subscriptions::Status).string().not_null())
                    .col(
                        ColumnDef::new(Subscriptions::PeriodStart)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Subscriptions::PeriodEnd)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Subscriptions::AutoRenew)
                            .boolean()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Subscriptions::Environment)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Subscriptions::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Subscriptions::Table, Subscriptions::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(UsageRecords::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(UsageRecords::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(UsageRecords::UserId).uuid().not_null())
                    .col(
                        ColumnDef::new(UsageRecords::CallId)
                            .uuid()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(UsageRecords::Bucket).string().not_null())
                    .col(ColumnDef::new(UsageRecords::PeriodStart).timestamp_with_time_zone())
                    .col(ColumnDef::new(UsageRecords::PeriodEnd).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(UsageRecords::ConnectedSeconds)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(UsageRecords::FinalizedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(UsageRecords::Table, UsageRecords::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(UsageRecords::Table, UsageRecords::CallId)
                            .to(Calls::Table, Calls::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_usage_user_bucket_period")
                    .table(UsageRecords::Table)
                    .col(UsageRecords::UserId)
                    .col(UsageRecords::Bucket)
                    .col(UsageRecords::PeriodStart)
                    .to_owned(),
            )
            .await?;

        // Preserve numbers bought by older builds as paid dedicated numbers.
        manager
            .get_connection()
            .execute_unprepared(
                "INSERT OR IGNORE INTO phone_numbers \
                 (id, twilio_sid, phone_number, friendly_name, country_code, purpose, status, \
                  dedicated_user_id, purchased_at) \
                 SELECT id, twilio_sid, phone_number, friendly_name, 'US', 'dedicated', 'active', \
                        user_id, created_at FROM owned_numbers",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(UsageRecords::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Subscriptions::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(VerifiedPhones::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(DeviceAttestations::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(AttestationChallenges::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(PhoneNumbers::Table).to_owned())
            .await
    }
}

struct CreateTables;

impl MigrationName for CreateTables {
    fn name(&self) -> &str {
        "m20260911_000001_create_tables"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CreateTables {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Calls::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Calls::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Calls::Direction).string().not_null())
                    .col(ColumnDef::new(Calls::PeerNumber).string().not_null())
                    .col(ColumnDef::new(Calls::TaskPrompt).text())
                    .col(ColumnDef::new(Calls::Status).string().not_null())
                    .col(ColumnDef::new(Calls::TwilioCallSid).string())
                    .col(ColumnDef::new(Calls::StartedAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(Calls::EndedAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(Calls::Summary).text())
                    .col(
                        ColumnDef::new(Calls::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(TranscriptSegments::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TranscriptSegments::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(TranscriptSegments::CallId).uuid().not_null())
                    .col(
                        ColumnDef::new(TranscriptSegments::Speaker)
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(TranscriptSegments::Text).text().not_null())
                    .col(
                        ColumnDef::new(TranscriptSegments::At)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(TranscriptSegments::Table, TranscriptSegments::CallId)
                            .to(Calls::Table, Calls::Id),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_transcript_segments_call_id")
                    .table(TranscriptSegments::Table)
                    .col(TranscriptSegments::CallId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(TranscriptSegments::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Calls::Table).to_owned())
            .await
    }
}

struct CreateAuthChatTables;

impl MigrationName for CreateAuthChatTables {
    fn name(&self) -> &str {
        "m20260911_000002_auth_chat_tables"
    }
}

struct CreateNumberOwnership;

impl MigrationName for CreateNumberOwnership {
    fn name(&self) -> &str {
        "m20260914_000003_number_ownership"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CreateNumberOwnership {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Calls::Table)
                    .add_column(ColumnDef::new(Calls::UserId).uuid())
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(OwnedNumbers::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(OwnedNumbers::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(OwnedNumbers::UserId)
                            .uuid()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(OwnedNumbers::TwilioSid)
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(OwnedNumbers::PhoneNumber)
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(OwnedNumbers::FriendlyName)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OwnedNumbers::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(OwnedNumbers::Table, OwnedNumbers::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_calls_user_id")
                    .table(Calls::Table)
                    .col(Calls::UserId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(OwnedNumbers::Table).to_owned())
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Calls::Table)
                    .drop_column(Calls::UserId)
                    .to_owned(),
            )
            .await
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CreateAuthChatTables {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Users::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Users::Id).uuid().not_null().primary_key())
                    .col(
                        ColumnDef::new(Users::Email)
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(Users::DisplayName).string().not_null())
                    .col(ColumnDef::new(Users::PasswordHash).string().not_null())
                    .col(
                        ColumnDef::new(Users::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(AuthTokens::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AuthTokens::Token)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(AuthTokens::UserId).uuid().not_null())
                    .col(
                        ColumnDef::new(AuthTokens::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(AuthTokens::Table, AuthTokens::UserId)
                            .to(Users::Table, Users::Id),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(ChatMessages::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ChatMessages::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(ChatMessages::UserId).uuid().not_null())
                    .col(ColumnDef::new(ChatMessages::Role).string().not_null())
                    .col(ColumnDef::new(ChatMessages::Content).text().not_null())
                    .col(
                        ColumnDef::new(ChatMessages::At)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(ChatMessages::Table, ChatMessages::UserId)
                            .to(Users::Table, Users::Id),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_chat_messages_user_id")
                    .table(ChatMessages::Table)
                    .col(ChatMessages::UserId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(ChatMessages::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(AuthTokens::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Users::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
    Email,
    DisplayName,
    PasswordHash,
    VoicePreference,
    AnnounceCourtside,
    CustomCallInstructions,
    TakeoverPhoneNumber,
    CurrentChatId,
    CreatedAt,
}

#[derive(DeriveIden)]
enum AuthTokens {
    Table,
    Token,
    UserId,
    CreatedAt,
}

#[derive(DeriveIden)]
enum ChatMessages {
    Table,
    Id,
    UserId,
    ChatId,
    Role,
    Content,
    At,
}

#[derive(DeriveIden)]
enum Calls {
    Table,
    Id,
    UserId,
    ChatId,
    Direction,
    PeerNumber,
    TaskPrompt,
    AiBackend,
    Status,
    TwilioCallSid,
    StartedAt,
    EndedAt,
    Summary,
    FromNumberId,
    UsageBucket,
    QuotaCapSeconds,
    ConnectedSeconds,
    UsageFinalized,
    AnnounceCourtside,
    ShareUserName,
    CreatedAt,
}

#[derive(DeriveIden)]
enum ChatSessions {
    Table,
    Id,
    UserId,
    Title,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum PhoneNumbers {
    Table,
    Id,
    TwilioSid,
    PhoneNumber,
    FriendlyName,
    CountryCode,
    Purpose,
    Status,
    DedicatedUserId,
    PurchasedAt,
    ReleaseDueAt,
    LastUsedAt,
    ReleasedAt,
    LastError,
}

#[derive(DeriveIden)]
enum VerifiedPhones {
    Table,
    Id,
    UserId,
    PhoneHash,
    MaskedPhone,
    LineType,
    CountryCode,
    CallerIdNumber,
    OutgoingCallerIdSid,
    VerifiedAt,
}

#[derive(DeriveIden)]
enum Subscriptions {
    Table,
    Id,
    UserId,
    ProductId,
    OriginalTransactionId,
    Status,
    PeriodStart,
    PeriodEnd,
    AutoRenew,
    Environment,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum UsageRecords {
    Table,
    Id,
    UserId,
    CallId,
    Bucket,
    PeriodStart,
    PeriodEnd,
    ConnectedSeconds,
    FinalizedAt,
}

#[derive(DeriveIden)]
enum AttestationChallenges {
    Table,
    Challenge,
    UserId,
    ExpiresAt,
    Used,
}

#[derive(DeriveIden)]
enum DeviceAttestations {
    Table,
    Id,
    UserId,
    KeyId,
    PublicKey,
    Receipt,
    DeviceToken,
    TrialPreviouslyUsed,
    AttestedAt,
}

#[derive(DeriveIden)]
enum OwnedNumbers {
    Table,
    Id,
    UserId,
    TwilioSid,
    PhoneNumber,
    FriendlyName,
    CreatedAt,
}

#[derive(DeriveIden)]
enum TranscriptSegments {
    Table,
    Id,
    CallId,
    Speaker,
    Text,
    At,
}

/// Preserve original chats and their index; remove the unwanted saved-facts store.
struct RemoveSavedMemory;
impl MigrationName for RemoveSavedMemory {
    fn name(&self) -> &str {
        "m20261011_000014_remove_saved_memory"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for RemoveSavedMemory {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for sql in [
            "CREATE TABLE chat_history_settings (user_id uuid NOT NULL PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE, enabled boolean NOT NULL DEFAULT true)",
            "INSERT INTO chat_history_settings(user_id,enabled) SELECT user_id,(enabled AND history_enabled) FROM memory_settings",
            "DROP TABLE saved_memories",
            "DROP TABLE memory_settings",
        ] { db.execute_unprepared(sql).await?; }
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // A rollback restores the old schema, never the deleted saved facts.
        CreateAccountMemory.up(manager).await?;
        let db = manager.get_connection();
        for sql in [
            "ALTER TABLE memory_settings ADD COLUMN history_enabled boolean NOT NULL DEFAULT true",
            "INSERT INTO memory_settings(user_id,enabled,history_enabled,revision) SELECT user_id,true,enabled,0 FROM chat_history_settings",
            "DROP TABLE chat_history_settings",
        ] { db.execute_unprepared(sql).await?; }
        Ok(())
    }
}
