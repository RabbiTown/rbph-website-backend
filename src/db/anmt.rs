use serde::Serialize;
use sqlx::prelude::FromRow;
use time::OffsetDateTime;

use crate::{DbPool, error::RbInternalError, model::game::RbContentType};

#[derive(FromRow, Serialize)]
pub struct RbAnnouncementShowData {
    pub id: i32,
    pub title: String,
    pub content: String,
    pub content_type: RbContentType,
    pub is_pinned: bool,
    pub game_id: Option<i32>,
    pub puzzle_id: Option<i32>,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub utime_at: OffsetDateTime,
}

pub async fn list_all_for_public(
    pool: &DbPool,
    game_id: i32,
) -> Result<Vec<RbAnnouncementShowData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbAnnouncementShowData,
        "SELECT a.id, a.title, a.content, a.content_type,
                a.is_pinned, a.game_id, a.puzzle_id, a.utime_at
        FROM rb_announcement a
        WHERE a.is_shown
            AND (a.game_id IS NULL OR a.game_id = $1)
            AND a.puzzle_id IS NULL
        ORDER BY a.is_pinned DESC, utime_at DESC",
        game_id
    )
    .fetch_all(pool)
    .await?;

    Ok(result)
}

pub async fn list_all_for_team(
    pool: &DbPool,
    team_id: i32,
) -> Result<Vec<RbAnnouncementShowData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbAnnouncementShowData,
        "SELECT a.id, a.title, a.content, a.content_type,
                a.is_pinned, a.game_id, a.puzzle_id, a.utime_at
        FROM rb_announcement a
        JOIN rb_team t ON t.id = $1
        WHERE a.is_shown
            AND (a.game_id IS NULL OR a.game_id = t.game_id)
            AND (a.puzzle_id IS NULL OR EXISTS (
                SELECT 1
                FROM rb_team_puzzle tp
                WHERE tp.team_id = t.id
                    AND tp.pstate >= 0
                    AND tp.puzzle_id = a.puzzle_id
            ))
        ORDER BY a.is_pinned DESC, utime_at DESC",
        team_id
    )
    .fetch_all(pool)
    .await?;

    Ok(result)
}
