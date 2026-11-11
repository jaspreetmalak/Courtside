//! Read-only retrieval from the owner's actual, completed conversations.
//! Indexed candidates are checked against normal chat history so legacy failed
//! drafts, foreign chats, and deleted messages never become model context.

use anyhow::Result;
use chrono::{DateTime, Utc};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, Statement,
};
use serde::Serialize;
use shared::{ChatMessage, ChatRole};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use super::{entities::chat_sessions, history_settings};

const RESULT_LIMIT: usize = 6;
const PAGE_SIZE: usize = 16;
const PAGE_CHARS: usize = 12_000;

#[derive(Debug, Serialize)]
pub struct Match {
    pub chat_id: Uuid,
    pub title: String,
    pub at: DateTime<Utc>,
    pub role: ChatRole,
    pub excerpt: String,
    /// Read from just before the match to resolve surrounding context.
    pub read_offset: usize,
}

#[derive(Debug, Serialize)]
pub struct Page {
    pub chat_id: Uuid,
    pub title: String,
    pub messages: Vec<ChatMessage>,
    pub offset: usize,
    pub next_offset: Option<usize>,
}

pub async fn belongs_to_user(
    db: &DatabaseConnection,
    chat_id: Uuid,
    user_id: Uuid,
) -> Result<bool> {
    Ok(chat_sessions::Entity::find_by_id(chat_id)
        .filter(chat_sessions::Column::UserId.eq(user_id))
        .one(db)
        .await?
        .is_some())
}

// Quote only plain terms, never accept FTS operators or SQL supplied by a model.
fn match_expression(query: &str) -> String {
    let mut seen = HashSet::new();
    query
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| word.chars().count() >= 2)
        .map(str::to_lowercase)
        .filter(|word| {
            !matches!(
                word.as_str(),
                "or" | "and" | "the" | "for" | "with" | "about" | "my" | "in" | "to"
            )
        })
        .filter(|word| seen.insert(word.clone()))
        .take(12)
        .map(|word| format!("\"{word}\"*"))
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn clip(text: &str, limit: usize) -> String {
    let mut chars = text.chars();
    let mut excerpt = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        excerpt.push('…');
    }
    excerpt
}

pub async fn search(db: &DatabaseConnection, user_id: Uuid, query: &str) -> Result<Vec<Match>> {
    if !history_settings::enabled(db, user_id).await? {
        return Ok(vec![]);
    }
    let query = query.chars().take(200).collect::<String>();
    let expression = match_expression(&query);
    let mut results = Vec::new();
    if query.trim().is_empty() {
        // A vague reference such as "our last conversation" can start with a
        // small recent-chat list. It does not change the owner's selected chat.
        let chats = chat_sessions::Entity::find()
            .filter(chat_sessions::Column::UserId.eq(user_id))
            .order_by_desc(chat_sessions::Column::UpdatedAt)
            .limit(12)
            .all(db)
            .await?;
        for chat in chats {
            let history = super::chat_history_for(db, user_id, chat.id).await?;
            if let Some(message) = history.last() {
                results.push(Match {
                    chat_id: chat.id,
                    title: chat.title,
                    at: message.at,
                    role: message.role.clone(),
                    excerpt: clip(&message.content, 700),
                    read_offset: history.len().saturating_sub(PAGE_SIZE),
                });
                if results.len() == RESULT_LIMIT {
                    break;
                }
            }
        }
    } else if !expression.is_empty() {
        let candidates = db.query_all(Statement::from_sql_and_values(DbBackend::Sqlite,
            "SELECT m.id, m.chat_id, c.title, snippet(chat_history_fts, 0, '', '', ' … ', 48) AS excerpt
             FROM chat_history_fts JOIN chat_messages m ON m.rowid = chat_history_fts.rowid
             JOIN chat_sessions c ON c.id = m.chat_id
             WHERE chat_history_fts MATCH ? AND m.user_id = ? AND c.user_id = ?
             ORDER BY bm25(chat_history_fts), m.at DESC LIMIT 60",
            [expression.into(), user_id.into(), user_id.into()])).await?;
        let mut histories = HashMap::new();
        let mut included = HashSet::new();
        for candidate in candidates {
            let chat_id: Uuid = candidate.try_get("", "chat_id")?;
            if included.contains(&chat_id) {
                continue;
            }
            if !histories.contains_key(&chat_id) {
                histories.insert(
                    chat_id,
                    super::chat_history_for(db, user_id, chat_id).await?,
                );
            }
            let history = &histories[&chat_id];
            let id: Uuid = candidate.try_get("", "id")?;
            let Some(index) = history.iter().position(|message| message.id == id) else {
                continue;
            };
            let message = &history[index];
            results.push(Match {
                chat_id,
                title: candidate.try_get("", "title")?,
                at: message.at,
                role: message.role.clone(),
                excerpt: clip(&candidate.try_get::<String>("", "excerpt")?, 700),
                read_offset: index.saturating_sub(2),
            });
            included.insert(chat_id);
            if results.len() == RESULT_LIMIT {
                break;
            }
        }
    }
    // Honor a control changed while the lookup was running.
    if !history_settings::enabled(db, user_id).await? {
        results.clear();
    }
    Ok(results)
}

pub async fn read(
    db: &DatabaseConnection,
    user_id: Uuid,
    chat_id: Uuid,
    offset: usize,
) -> Result<Option<Page>> {
    if !history_settings::enabled(db, user_id).await? {
        return Ok(None);
    }
    let Some(chat) = chat_sessions::Entity::find_by_id(chat_id)
        .filter(chat_sessions::Column::UserId.eq(user_id))
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let history = super::chat_history_for(db, user_id, chat_id).await?;
    let mut messages = Vec::new();
    let mut characters = 0;
    for source in history.iter().skip(offset).take(PAGE_SIZE) {
        let mut message = source.clone();
        message.content = clip(&message.content, 4_000);
        let size = message.content.chars().count();
        if !messages.is_empty() && characters + size > PAGE_CHARS {
            break;
        }
        characters += size;
        messages.push(message);
    }
    let end = offset.saturating_add(messages.len());
    if !history_settings::enabled(db, user_id).await? {
        return Ok(None);
    }
    Ok(Some(Page {
        chat_id,
        title: chat.title,
        messages,
        offset,
        next_offset: (end < history.len()).then_some(end),
    }))
}

#[cfg(test)]
mod tests {
    use super::super::entities::chat_messages;
    use super::*;

    #[tokio::test]
    async fn saved_facts_migration_keeps_original_chats_and_history_opt_out() {
        use sea_orm_migration::MigratorTrait;
        let db = super::super::connect("sqlite::memory:").await.unwrap();
        let owner = super::super::create_user(&db, "remove-facts@example.test", "Owner", "unused")
            .await
            .unwrap();
        let chat = super::super::ensure_current_chat(&db, owner.id)
            .await
            .unwrap();
        super::super::add_chat_exchange(
            &db,
            owner.id,
            chat.id,
            "My dental reference is ZX-4827.",
            "Got it.",
        )
        .await
        .unwrap();
        super::super::migration::Migrator::down(&db, Some(1))
            .await
            .unwrap();
        db.execute_unprepared("INSERT INTO memory_settings(user_id,enabled,history_enabled,revision) SELECT id,false,true,0 FROM users").await.unwrap();
        db.execute_unprepared("INSERT INTO saved_memories(id,user_id,key,content,created_at,updated_at) SELECT 'aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa',id,'dining','vegetarian','2026-10-01','2026-10-01' FROM users").await.unwrap();
        super::super::migration::Migrator::up(&db, None)
            .await
            .unwrap();
        let tables=db.query_all(Statement::from_string(DbBackend::Sqlite,"SELECT name FROM sqlite_master WHERE type='table' AND name IN ('saved_memories','memory_settings')")).await.unwrap();
        assert!(tables.is_empty());
        assert!(!history_settings::enabled(&db, owner.id).await.unwrap());
        assert_eq!(
            super::super::chat_history_for(&db, owner.id, chat.id)
                .await
                .unwrap()
                .len(),
            2
        );
        history_settings::set_enabled(&db, owner.id, true)
            .await
            .unwrap();
        assert_eq!(
            search(&db, owner.id, "dental").await.unwrap()[0].chat_id,
            chat.id
        );
    }
    use sea_orm::{ActiveModelTrait, ActiveValue::Set};

    #[tokio::test]
    async fn retrieves_real_old_exchanges_without_saved_memories_and_respects_ownership_and_controls(
    ) {
        let db = super::super::connect("sqlite::memory:").await.unwrap();
        let owner = super::super::create_user(&db, "history-owner@example.test", "Owner", "unused")
            .await
            .unwrap();
        let other = super::super::create_user(&db, "history-other@example.test", "Other", "unused")
            .await
            .unwrap();
        let first = super::super::ensure_current_chat(&db, owner.id)
            .await
            .unwrap();
        super::super::add_chat_exchange(
            &db,
            owner.id,
            first.id,
            "Help plan a Cedar Dental appointment. My booking reference is ZX-4827.",
            "We can ask Cedar Dental about an afternoon appointment.",
        )
        .await
        .unwrap();
        let fresh = super::super::start_new_chat(&db, owner.id).await.unwrap();
        let foreign = super::super::ensure_current_chat(&db, other.id)
            .await
            .unwrap();
        super::super::add_chat_exchange(
            &db,
            other.id,
            foreign.id,
            "Cedar Dental secret reference FOREIGN-999",
            "Secret",
        )
        .await
        .unwrap();
        let draft = chat_messages::ActiveModel {
            id: Set(Uuid::new_v4()),
            user_id: Set(owner.id),
            chat_id: Set(Some(fresh.id)),
            role: Set("user".into()),
            content: Set("Failed Cedar Dental draft with WRONG-000".into()),
            at: Set(Utc::now()),
        }
        .insert(&db)
        .await
        .unwrap();
        let matches = search(&db, owner.id, "Cedar dentist").await.unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].chat_id, first.id);
        let page = read(&db, owner.id, first.id, matches[0].read_offset)
            .await
            .unwrap()
            .unwrap();
        let text = serde_json::to_string(&page).unwrap();
        assert!(text.contains("ZX-4827"));
        assert!(!text.contains("FOREIGN") && !text.contains("WRONG"));
        assert!(read(&db, other.id, first.id, 0).await.unwrap().is_none());
        assert_eq!(
            super::super::ensure_current_chat(&db, owner.id)
                .await
                .unwrap()
                .id,
            fresh.id
        );
        assert!(search(&db, owner.id, "xyzunknown")
            .await
            .unwrap()
            .is_empty());
        assert!(search(&db, owner.id, "\" OR * NOT :")
            .await
            .unwrap()
            .is_empty());
        history_settings::set_enabled(&db, owner.id, false)
            .await
            .unwrap();
        assert!(search(&db, owner.id, "Cedar").await.unwrap().is_empty());
        assert!(read(&db, owner.id, first.id, 0).await.unwrap().is_none());
        history_settings::set_enabled(&db, owner.id, true)
            .await
            .unwrap();
        assert!(!search(&db, owner.id, "Cedar").await.unwrap().is_empty());
        chat_messages::Entity::delete_by_id(draft.id)
            .exec(&db)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn index_backfills_existing_chats_and_tracks_edits_and_deletes() {
        use sea_orm_migration::MigratorTrait;
        let db = super::super::connect("sqlite::memory:").await.unwrap();
        let owner =
            super::super::create_user(&db, "history-migration@example.test", "Owner", "unused")
                .await
                .unwrap();
        // Remove only the new migration to recreate a pre-update database.
        super::super::migration::Migrator::down(&db, Some(2))
            .await
            .unwrap();
        let chat = super::super::ensure_current_chat(&db, owner.id)
            .await
            .unwrap();
        let (message, _) = super::super::add_chat_exchange(
            &db,
            owner.id,
            chat.id,
            "Old Juniper restaurant booking reference 7421",
            "Booking plan",
        )
        .await
        .unwrap();
        super::super::migration::Migrator::up(&db, None)
            .await
            .unwrap();
        assert_eq!(search(&db, owner.id, "Juniper").await.unwrap().len(), 1);
        let mut edited: chat_messages::ActiveModel = chat_messages::Entity::find_by_id(message.id)
            .one(&db)
            .await
            .unwrap()
            .unwrap()
            .into();
        edited.content = Set("Old Willow restaurant booking reference 7421".into());
        edited.update(&db).await.unwrap();
        assert!(search(&db, owner.id, "Juniper").await.unwrap().is_empty());
        assert_eq!(search(&db, owner.id, "Willow").await.unwrap().len(), 1);
        chat_messages::Entity::delete_by_id(message.id)
            .exec(&db)
            .await
            .unwrap();
        assert!(search(&db, owner.id, "Willow").await.unwrap().is_empty());
        assert!(read(&db, owner.id, chat.id, 0)
            .await
            .unwrap()
            .unwrap()
            .messages
            .is_empty());
    }

    #[tokio::test]
    async fn reads_long_conversations_in_bounded_pages() {
        let db = super::super::connect("sqlite::memory:").await.unwrap();
        let owner = super::super::create_user(&db, "history-pages@example.test", "Owner", "unused")
            .await
            .unwrap();
        let chat = super::super::ensure_current_chat(&db, owner.id)
            .await
            .unwrap();
        for index in 0..20 {
            super::super::add_chat_exchange(
                &db,
                owner.id,
                chat.id,
                &format!("Booking detail {index} {}", "A".repeat(1_500)),
                "Noted",
            )
            .await
            .unwrap();
        }
        let mut offset = 0;
        let mut seen = HashSet::new();
        loop {
            let page = read(&db, owner.id, chat.id, offset).await.unwrap().unwrap();
            assert!(page.messages.len() <= PAGE_SIZE);
            assert!(
                page.messages
                    .iter()
                    .map(|message| message.content.chars().count())
                    .sum::<usize>()
                    <= PAGE_CHARS
            );
            for message in page.messages {
                assert!(seen.insert(message.id));
            }
            match page.next_offset {
                Some(next) => {
                    assert!(next > offset);
                    offset = next;
                }
                None => break,
            }
        }
        assert_eq!(seen.len(), 40);
    }
}
