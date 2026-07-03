use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sqlx::prelude::FromRow;
use time::OffsetDateTime;

use crate::{DbPool, error::RbInternalError, model::game::RbContentType};

#[derive(Clone, FromRow, Serialize)]
pub struct AnnouncementPuzzleData {
    pub id: i32,
    pub slug: Option<String>,
    pub title: String,
    pub round_id: i32,
    pub round_slug: Option<String>,
    pub is_round_puzzle: bool,
}

#[derive(Serialize)]
pub struct RbAnnouncementShowData {
    pub id: i32,
    pub title: String,
    pub content: String,
    pub content_type: RbContentType,
    pub is_pinned: bool,
    pub game_id: Option<i32>,
    pub puzzles: Vec<AnnouncementPuzzleData>,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub utime_at: OffsetDateTime,
}

#[derive(Serialize)]
pub struct AdminAnnouncementData {
    pub id: i32,
    pub title: String,
    pub content: String,
    pub content_type: RbContentType,
    pub is_pinned: bool,
    pub is_shown: bool,
    pub game_id: Option<i32>,
    pub puzzles: Vec<AnnouncementPuzzleData>,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub ctime_at: OffsetDateTime,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub utime_at: OffsetDateTime,
}

#[derive(Deserialize)]
pub struct AnnouncementWriteData {
    pub title: String,
    pub content: String,
    pub content_type: RbContentType,
    pub is_pinned: bool,
    pub is_shown: bool,
    pub game_id: Option<i32>,
    pub puzzle_ids: Vec<i32>,
}

#[derive(FromRow)]
struct AnnouncementShowRow {
    id: i32,
    title: String,
    content: String,
    content_type: RbContentType,
    is_pinned: bool,
    game_id: Option<i32>,
    utime_at: OffsetDateTime,
}

#[derive(FromRow)]
struct AdminAnnouncementRow {
    id: i32,
    title: String,
    content: String,
    content_type: RbContentType,
    is_pinned: bool,
    is_shown: bool,
    game_id: Option<i32>,
    ctime_at: OffsetDateTime,
    utime_at: OffsetDateTime,
}

#[derive(FromRow)]
struct AnnouncementPuzzleRow {
    announcement_id: i32,
    id: i32,
    slug: Option<String>,
    title: String,
    round_id: i32,
    round_slug: Option<String>,
    is_round_puzzle: bool,
}

async fn load_puzzles(
    pool: &DbPool,
    announcement_ids: &[i32],
    team_id: Option<i32>,
) -> Result<HashMap<i32, Vec<AnnouncementPuzzleData>>, RbInternalError> {
    if announcement_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let rows = if let Some(team_id) = team_id {
        sqlx::query_as!(
            AnnouncementPuzzleRow,
            "SELECT ap.announcement_id, p.id, p.slug, p.title, p.round_id,
                r.slug AS round_slug, (r.puzzle = p.id) AS \"is_round_puzzle!\"
            FROM rb_announcement_puzzle ap
            JOIN rb_puzzle p ON p.id = ap.puzzle_id
            JOIN rb_round r ON r.id = p.round_id
            JOIN rb_release_phase rp ON rp.id = p.release_phase_id AND rp.release_at <= NOW()
            JOIN rb_team_puzzle tp ON tp.puzzle_id = p.id AND tp.team_id = $2 AND tp.state >= 0
            WHERE ap.announcement_id = ANY($1)
            ORDER BY r.sort, r.id, (p.id IS DISTINCT FROM r.puzzle), p.sort, p.id;",
            announcement_ids,
            team_id
        )
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as!(
            AnnouncementPuzzleRow,
            "SELECT ap.announcement_id, p.id, p.slug, p.title, p.round_id,
                r.slug AS round_slug, (r.puzzle = p.id) AS \"is_round_puzzle!\"
            FROM rb_announcement_puzzle ap
            JOIN rb_puzzle p ON p.id = ap.puzzle_id
            JOIN rb_round r ON r.id = p.round_id
            WHERE ap.announcement_id = ANY($1)
            ORDER BY r.sort, r.id, (p.id IS DISTINCT FROM r.puzzle), p.sort, p.id;",
            announcement_ids
        )
        .fetch_all(pool)
        .await?
    };

    let mut result = HashMap::new();
    for row in rows {
        result
            .entry(row.announcement_id)
            .or_insert_with(Vec::new)
            .push(AnnouncementPuzzleData {
                id: row.id,
                slug: row.slug,
                title: row.title,
                round_id: row.round_id,
                round_slug: row.round_slug,
                is_round_puzzle: row.is_round_puzzle,
            });
    }
    Ok(result)
}

async fn attach_show_puzzles(
    pool: &DbPool,
    rows: Vec<AnnouncementShowRow>,
    team_id: Option<i32>,
) -> Result<Vec<RbAnnouncementShowData>, RbInternalError> {
    let ids = rows.iter().map(|row| row.id).collect::<Vec<_>>();
    let mut puzzles = load_puzzles(pool, &ids, team_id).await?;
    Ok(rows
        .into_iter()
        .map(|row| RbAnnouncementShowData {
            id: row.id,
            title: row.title,
            content: row.content,
            content_type: row.content_type,
            is_pinned: row.is_pinned,
            game_id: row.game_id,
            puzzles: puzzles.remove(&row.id).unwrap_or_default(),
            utime_at: row.utime_at,
        })
        .collect())
}

async fn attach_admin_puzzles(
    pool: &DbPool,
    rows: Vec<AdminAnnouncementRow>,
) -> Result<Vec<AdminAnnouncementData>, RbInternalError> {
    let ids = rows.iter().map(|row| row.id).collect::<Vec<_>>();
    let mut puzzles = load_puzzles(pool, &ids, None).await?;
    Ok(rows
        .into_iter()
        .map(|row| AdminAnnouncementData {
            id: row.id,
            title: row.title,
            content: row.content,
            content_type: row.content_type,
            is_pinned: row.is_pinned,
            is_shown: row.is_shown,
            game_id: row.game_id,
            puzzles: puzzles.remove(&row.id).unwrap_or_default(),
            ctime_at: row.ctime_at,
            utime_at: row.utime_at,
        })
        .collect())
}

pub async fn list_all_for_public(
    pool: &DbPool,
    game_id: i32,
) -> Result<Vec<RbAnnouncementShowData>, RbInternalError> {
    let rows = sqlx::query_as!(
        AnnouncementShowRow,
        "SELECT a.id, a.title, a.content, a.content_type,
            a.is_pinned, a.game_id, a.utime_at
        FROM rb_announcement a
        WHERE a.is_shown
            AND (a.game_id IS NULL OR a.game_id = $1)
            AND NOT EXISTS (
                SELECT 1 FROM rb_announcement_puzzle ap WHERE ap.announcement_id = a.id
            )
        ORDER BY a.is_pinned DESC, a.utime_at DESC;",
        game_id
    )
    .fetch_all(pool)
    .await?;
    attach_show_puzzles(pool, rows, None).await
}

pub async fn list_all_for_team(
    pool: &DbPool,
    team_id: i32,
) -> Result<Vec<RbAnnouncementShowData>, RbInternalError> {
    let rows = sqlx::query_as!(
        AnnouncementShowRow,
        "SELECT a.id, a.title, a.content, a.content_type,
            a.is_pinned, a.game_id, a.utime_at
        FROM rb_announcement a
        JOIN rb_team t ON t.id = $1
        WHERE a.is_shown
            AND (a.game_id IS NULL OR a.game_id = t.game_id)
            AND (
                NOT EXISTS (
                    SELECT 1 FROM rb_announcement_puzzle ap WHERE ap.announcement_id = a.id
                )
                OR EXISTS (
                    SELECT 1
                    FROM rb_announcement_puzzle ap
                    JOIN rb_puzzle p ON p.id = ap.puzzle_id
                    JOIN rb_release_phase rp ON rp.id = p.release_phase_id AND rp.release_at <= NOW()
                    JOIN rb_team_puzzle tp ON tp.puzzle_id = p.id
                        AND tp.team_id = t.id AND tp.state >= 0
                    WHERE ap.announcement_id = a.id
                )
            )
        ORDER BY a.is_pinned DESC, a.utime_at DESC;",
        team_id
    )
    .fetch_all(pool)
    .await?;
    attach_show_puzzles(pool, rows, Some(team_id)).await
}

pub async fn list_for_team_puzzle(
    pool: &DbPool,
    team_id: i32,
    puzzle_id: i32,
) -> Result<Vec<RbAnnouncementShowData>, RbInternalError> {
    let rows = sqlx::query_as!(
        AnnouncementShowRow,
        "SELECT a.id, a.title, a.content, a.content_type,
            a.is_pinned, a.game_id, a.utime_at
        FROM rb_announcement a
        JOIN rb_announcement_puzzle current_ap
            ON current_ap.announcement_id = a.id AND current_ap.puzzle_id = $2
        JOIN rb_team t ON t.id = $1 AND t.game_id = a.game_id
        WHERE a.is_shown
        ORDER BY a.is_pinned DESC, a.utime_at DESC;",
        team_id,
        puzzle_id
    )
    .fetch_all(pool)
    .await?;
    attach_show_puzzles(pool, rows, Some(team_id)).await
}

pub async fn admin_list(
    pool: &DbPool,
    game_id: Option<i32>,
) -> Result<Vec<AdminAnnouncementData>, RbInternalError> {
    let rows = sqlx::query_as!(
        AdminAnnouncementRow,
        "SELECT a.id, a.title, a.content, a.content_type, a.is_pinned, a.is_shown,
            a.game_id, a.ctime_at, a.utime_at
        FROM rb_announcement a
        WHERE ($1::INT IS NULL AND a.game_id IS NULL)
            OR ($1::INT IS NOT NULL AND a.game_id = $1)
        ORDER BY a.is_pinned DESC, a.utime_at DESC, a.id DESC;",
        game_id
    )
    .fetch_all(pool)
    .await?;
    attach_admin_puzzles(pool, rows).await
}

pub async fn admin_get(
    pool: &DbPool,
    announcement_id: i32,
) -> Result<Option<AdminAnnouncementData>, RbInternalError> {
    let row = sqlx::query_as!(
        AdminAnnouncementRow,
        "SELECT a.id, a.title, a.content, a.content_type, a.is_pinned, a.is_shown,
            a.game_id, a.ctime_at, a.utime_at
        FROM rb_announcement a
        WHERE a.id = $1;",
        announcement_id
    )
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(attach_admin_puzzles(pool, vec![row]).await?.pop())
}

async fn replace_puzzles(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    announcement_id: i32,
    puzzle_ids: &[i32],
) -> Result<(), RbInternalError> {
    sqlx::query!(
        "DELETE FROM rb_announcement_puzzle WHERE announcement_id = $1;",
        announcement_id
    )
    .execute(&mut **tx)
    .await?;
    if !puzzle_ids.is_empty() {
        sqlx::query!(
            "INSERT INTO rb_announcement_puzzle (announcement_id, puzzle_id)
            SELECT $1, UNNEST($2::INT[]);",
            announcement_id,
            puzzle_ids
        )
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

pub async fn admin_create(
    pool: &DbPool,
    data: &AnnouncementWriteData,
) -> Result<AdminAnnouncementData, RbInternalError> {
    let mut tx = pool.begin().await?;
    let id = sqlx::query_scalar!(
        "INSERT INTO rb_announcement (
            title, content, content_type, is_pinned, is_shown, game_id
        ) VALUES ($1, $2, $3, $4, $5, $6)
        RETURNING id;",
        data.title.trim(),
        data.content,
        i16::from(data.content_type),
        data.is_pinned,
        data.is_shown,
        data.game_id
    )
    .fetch_one(&mut *tx)
    .await?;
    replace_puzzles(&mut tx, id, &data.puzzle_ids).await?;
    tx.commit().await?;
    admin_get(pool, id)
        .await?
        .ok_or_else(|| "Created announcement not found".into())
}

pub async fn admin_update(
    pool: &DbPool,
    announcement_id: i32,
    data: &AnnouncementWriteData,
) -> Result<Option<AdminAnnouncementData>, RbInternalError> {
    let mut tx = pool.begin().await?;
    let updated = sqlx::query_scalar!(
        "UPDATE rb_announcement
        SET title = $2, content = $3, content_type = $4,
            is_pinned = $5, is_shown = $6, game_id = $7, utime_at = NOW()
        WHERE id = $1
        RETURNING id;",
        announcement_id,
        data.title.trim(),
        data.content,
        i16::from(data.content_type),
        data.is_pinned,
        data.is_shown,
        data.game_id
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(id) = updated else {
        tx.commit().await?;
        return Ok(None);
    };
    replace_puzzles(&mut tx, id, &data.puzzle_ids).await?;
    tx.commit().await?;
    admin_get(pool, id).await
}

pub async fn admin_delete(pool: &DbPool, announcement_id: i32) -> Result<bool, RbInternalError> {
    Ok(sqlx::query_scalar!(
        "DELETE FROM rb_announcement WHERE id = $1 RETURNING id;",
        announcement_id
    )
    .fetch_optional(pool)
    .await?
    .is_some())
}
