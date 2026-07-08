use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use time::OffsetDateTime;

use crate::{
    DbPool,
    db::feature::{FeatureChangeData, GameFeature},
    error::RbInternalError,
    model::game::RbContentType,
};

pub const RELEASE_VISIBILITY_HIDDEN: i16 = 0;
pub const RELEASE_VISIBILITY_PUBLIC: i16 = 1;
pub const RELEASE_EVENT_PHASE: i16 = 0;
pub const RELEASE_EVENT_IMMEDIATE_PUZZLES: i16 = 1;

#[derive(Serialize)]
pub struct ReleasePhaseAdminData {
    pub id: i32,
    pub game_id: i32,
    pub title: String,
    pub description: String,
    pub content_type: RbContentType,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub release_at: OffsetDateTime,
    pub visibility: i16,
    pub feature_changes: Vec<FeatureChangeData>,
    pub puzzle_count: i64,
    pub released: bool,
}

struct ReleasePhaseAdminRow {
    id: i32,
    game_id: i32,
    title: String,
    description: String,
    content_type: RbContentType,
    release_at: OffsetDateTime,
    visibility: i16,
    puzzle_count: i64,
    released: bool,
}

#[derive(Deserialize)]
pub struct ReleasePhaseCreateData {
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub content_type: RbContentType,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub release_at: OffsetDateTime,
    pub visibility: i16,
    #[serde(default)]
    pub feature_changes: Vec<FeatureChangeData>,
}

#[derive(Default, Deserialize)]
pub struct ReleasePhaseUpdateData {
    pub title: Option<String>,
    pub description: Option<String>,
    pub content_type: Option<RbContentType>,
    #[serde(
        default,
        with = "crate::serde_helpers::serialize_option_offset_datetime"
    )]
    pub release_at: Option<OffsetDateTime>,
    pub visibility: Option<i16>,
    pub feature_changes: Option<Vec<FeatureChangeData>>,
}

#[derive(Clone)]
pub struct PendingReleaseEvent {
    pub id: i64,
    pub phase_id: Option<i32>,
    pub game_id: i32,
    pub occurred_at: OffsetDateTime,
}

#[derive(Serialize)]
pub struct ReleasedPuzzleData {
    pub id: i32,
    pub slug: Option<String>,
    pub title: String,
    pub round_id: i32,
    pub round_slug: Option<String>,
    pub is_round_puzzle: bool,
}

#[derive(Serialize)]
pub struct PlayerReleasePhaseData {
    pub id: i32,
    pub title: String,
    pub description: String,
    pub content_type: RbContentType,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub release_at: OffsetDateTime,
}

struct PlayerReleasePhaseRow {
    id: i32,
    title: String,
    description: String,
    content_type: RbContentType,
    release_at: OffsetDateTime,
}

#[derive(Serialize)]
pub struct ReleaseSyncEvent {
    pub id: i64,
    #[serde(rename = "type")]
    pub event_type: &'static str,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub occurred_at: OffsetDateTime,
    pub phase: Option<PlayerReleasePhaseData>,
    pub puzzles: Vec<ReleasedPuzzleData>,
}

pub struct ReleaseSyncResult {
    pub cursor: i64,
    pub events: Vec<ReleaseSyncEvent>,
}

async fn load_changes(
    pool: &DbPool,
    phase_ids: &[i32],
) -> Result<HashMap<i32, Vec<FeatureChangeData>>, RbInternalError> {
    if phase_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = sqlx::query!(
        "SELECT phase_id, feature_type, target_state
        FROM rb_release_phase_feature_change
        WHERE phase_id = ANY($1)
        ORDER BY feature_type;",
        phase_ids
    )
    .fetch_all(pool)
    .await?;
    let mut result: HashMap<i32, Vec<FeatureChangeData>> = HashMap::new();
    for row in rows {
        let feature = GameFeature::from_value(row.feature_type).ok_or("Invalid game feature")?;
        result
            .entry(row.phase_id)
            .or_default()
            .push(FeatureChangeData {
                feature,
                state: feature
                    .decode_state(row.target_state)
                    .ok_or("Invalid game feature state")?,
            });
    }
    Ok(result)
}

pub async fn list_admin(
    pool: &DbPool,
    game_id: i32,
) -> Result<Vec<ReleasePhaseAdminData>, RbInternalError> {
    let rows = sqlx::query_as!(
        ReleasePhaseAdminRow,
        "SELECT rp.id, rp.game_id, rp.title, rp.description,
            rp.content_type AS \"content_type: RbContentType\", rp.release_at, rp.visibility,
            COUNT(p.id) AS \"puzzle_count!\", (re.id IS NOT NULL) AS \"released!\"
        FROM rb_release_phase rp
        LEFT JOIN rb_release_event re ON re.phase_id = rp.id
        LEFT JOIN rb_puzzle p ON p.release_phase_id = rp.id
        WHERE rp.game_id = $1
        GROUP BY rp.id, re.id
        ORDER BY rp.release_at, rp.id;",
        game_id
    )
    .fetch_all(pool)
    .await?;
    let phase_ids = rows.iter().map(|row| row.id).collect::<Vec<_>>();
    let mut changes = load_changes(pool, &phase_ids).await?;
    Ok(rows
        .into_iter()
        .map(|row| ReleasePhaseAdminData {
            id: row.id,
            game_id: row.game_id,
            title: row.title,
            description: row.description,
            content_type: row.content_type,
            release_at: row.release_at,
            visibility: row.visibility,
            feature_changes: changes.remove(&row.id).unwrap_or_default(),
            puzzle_count: row.puzzle_count,
            released: row.released,
        })
        .collect())
}

pub async fn get_admin(
    pool: &DbPool,
    game_id: i32,
    phase_id: i32,
) -> Result<Option<ReleasePhaseAdminData>, RbInternalError> {
    Ok(list_admin(pool, game_id)
        .await?
        .into_iter()
        .find(|phase| phase.id == phase_id))
}

async fn replace_changes_conn(
    conn: &mut PgConnection,
    game_id: i32,
    phase_id: i32,
    changes: &[FeatureChangeData],
) -> Result<(), RbInternalError> {
    sqlx::query!(
        "DELETE FROM rb_release_phase_feature_change WHERE phase_id = $1;",
        phase_id
    )
    .execute(&mut *conn)
    .await?;
    for change in changes {
        sqlx::query!(
            "INSERT INTO rb_release_phase_feature_change (
                phase_id, game_id, feature_type, target_state
            ) VALUES ($1, $2, $3, $4);",
            phase_id,
            game_id,
            change.feature.value(),
            change
                .feature
                .encode_state(change.state)
                .ok_or("Invalid game feature state")?
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

pub async fn create_admin(
    pool: &DbPool,
    game_id: i32,
    data: &ReleasePhaseCreateData,
) -> Result<Option<ReleasePhaseAdminData>, RbInternalError> {
    let mut tx = pool.begin().await?;
    let id = sqlx::query_scalar!(
        "INSERT INTO rb_release_phase (
            game_id, title, description, content_type, release_at, visibility
        ) SELECT id, $2, $3, $4, $5, $6 FROM rb_game WHERE id = $1
        RETURNING id;",
        game_id,
        data.title.trim(),
        data.description.trim(),
        i16::from(data.content_type),
        data.release_at,
        data.visibility
    )
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(id) = id {
        replace_changes_conn(&mut tx, game_id, id, &data.feature_changes).await?;
    }
    tx.commit().await?;
    match id {
        Some(id) => get_admin(pool, game_id, id).await,
        None => Ok(None),
    }
}

pub async fn update_admin(
    pool: &DbPool,
    game_id: i32,
    phase_id: i32,
    data: &ReleasePhaseUpdateData,
) -> Result<Option<ReleasePhaseAdminData>, RbInternalError> {
    let mut tx = pool.begin().await?;
    let updated = sqlx::query_scalar!(
        "UPDATE rb_release_phase rp
        SET title = COALESCE($3, rp.title), description = COALESCE($4, rp.description),
            content_type = COALESCE($5, rp.content_type),
            release_at = COALESCE($6, rp.release_at), visibility = COALESCE($7, rp.visibility)
        WHERE rp.game_id = $1 AND rp.id = $2
        RETURNING rp.id;",
        game_id,
        phase_id,
        data.title.as_deref().map(str::trim),
        data.description.as_deref().map(str::trim),
        data.content_type.map(i16::from),
        data.release_at,
        data.visibility
    )
    .fetch_optional(&mut *tx)
    .await?;
    if updated.is_some()
        && let Some(changes) = &data.feature_changes
    {
        replace_changes_conn(&mut tx, game_id, phase_id, changes).await?;
    }
    tx.commit().await?;
    match updated {
        Some(id) => get_admin(pool, game_id, id).await,
        None => Ok(None),
    }
}

pub async fn delete_admin(
    pool: &DbPool,
    game_id: i32,
    phase_id: i32,
) -> Result<bool, RbInternalError> {
    let result = sqlx::query!(
        "DELETE FROM rb_release_phase rp
        WHERE rp.game_id = $1 AND rp.id = $2
            AND NOT EXISTS (SELECT 1 FROM rb_puzzle p WHERE p.release_phase_id = rp.id);",
        game_id,
        phase_id
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn materialize_due(pool: &DbPool) -> Result<(), RbInternalError> {
    sqlx::query!(
        "INSERT INTO rb_release_event (phase_id, game_id, event_type, occurred_at)
        SELECT rp.id, rp.game_id, 0, rp.release_at FROM rb_release_phase rp
        WHERE rp.release_at <= NOW()
        ORDER BY rp.release_at, rp.id
        ON CONFLICT (phase_id) DO NOTHING;"
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn pending_notifications(
    pool: &DbPool,
) -> Result<Vec<PendingReleaseEvent>, RbInternalError> {
    Ok(sqlx::query_as!(
        PendingReleaseEvent,
        "SELECT re.id, re.phase_id, re.game_id, re.occurred_at
        FROM rb_release_event re
        WHERE re.notified_at IS NULL
        ORDER BY re.id;"
    )
    .fetch_all(pool)
    .await?)
}

pub async fn mark_notified(pool: &DbPool, event_id: i64) -> Result<(), RbInternalError> {
    sqlx::query!(
        "UPDATE rb_release_event SET notified_at = NOW()
        WHERE id = $1 AND notified_at IS NULL;",
        event_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn mark_content_blocks_dirty(
    pool: &DbPool,
    event_id: i64,
    phase_id: Option<i32>,
) -> Result<(), RbInternalError> {
    sqlx::query!(
        "UPDATE rb_team t SET content_blocks_dirty = TRUE
        WHERE EXISTS (
            SELECT 1 FROM rb_team_puzzle tp
            JOIN rb_puzzle p ON p.id = tp.puzzle_id
            LEFT JOIN rb_release_event_puzzle_team rept
                ON rept.event_id = $1 AND rept.puzzle_id = p.id AND rept.team_id = tp.team_id
            WHERE tp.team_id = t.id AND tp.state >= 0
                AND (($2::INT IS NOT NULL AND p.release_phase_id = $2)
                    OR rept.event_id IS NOT NULL)
        );",
        event_id,
        phase_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn next_delay_seconds(pool: &DbPool) -> Result<Option<u64>, RbInternalError> {
    let seconds = sqlx::query_scalar!(
        "SELECT CEIL(EXTRACT(EPOCH FROM (MIN(rp.release_at) - NOW())))::BIGINT
        FROM rb_release_phase rp
        WHERE NOT EXISTS (SELECT 1 FROM rb_release_event re WHERE re.phase_id = rp.id);"
    )
    .fetch_one(pool)
    .await?;
    Ok(seconds.and_then(|value| u64::try_from(value.max(0)).ok()))
}

pub async fn release_cursor(pool: &DbPool, game_id: i32) -> Result<i64, RbInternalError> {
    Ok(sqlx::query_scalar!(
        "SELECT COALESCE(MAX(re.id), 0) AS \"cursor!\"
        FROM rb_release_event re
        WHERE re.game_id = $1;",
        game_id
    )
    .fetch_one(pool)
    .await?)
}

fn player_phase_from_row(row: PlayerReleasePhaseRow) -> PlayerReleasePhaseData {
    PlayerReleasePhaseData {
        id: row.id,
        title: row.title,
        description: row.description,
        content_type: row.content_type,
        release_at: row.release_at,
    }
}

pub async fn list_player(
    pool: &DbPool,
    game_id: i32,
) -> Result<Vec<PlayerReleasePhaseData>, RbInternalError> {
    let rows = sqlx::query_as!(
        PlayerReleasePhaseRow,
        "SELECT rp.id, rp.title, rp.description,
            rp.content_type AS \"content_type: RbContentType\", rp.release_at
        FROM rb_release_phase rp
        LEFT JOIN rb_release_event re ON re.phase_id = rp.id
        WHERE rp.game_id = $1
            AND (rp.visibility = $2 OR re.id IS NOT NULL OR rp.release_at <= NOW())
        ORDER BY rp.release_at, rp.id;",
        game_id,
        RELEASE_VISIBILITY_PUBLIC
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(player_phase_from_row).collect())
}

async fn get_player_phase(
    pool: &DbPool,
    phase_id: i32,
) -> Result<Option<PlayerReleasePhaseData>, RbInternalError> {
    let row = sqlx::query_as!(
        PlayerReleasePhaseRow,
        "SELECT rp.id, rp.title, rp.description,
            rp.content_type AS \"content_type: RbContentType\", rp.release_at
        FROM rb_release_phase rp WHERE rp.id = $1;",
        phase_id
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(player_phase_from_row))
}

pub async fn sync_events(
    pool: &DbPool,
    game_id: i32,
    team_id: Option<i32>,
    after: i64,
) -> Result<ReleaseSyncResult, RbInternalError> {
    let rows = sqlx::query!(
        "SELECT re.id, re.occurred_at, re.phase_id, re.event_type
        FROM rb_release_event re
        WHERE re.game_id = $1 AND re.id > $2
        ORDER BY re.id LIMIT 100;",
        game_id,
        after.max(0)
    )
    .fetch_all(pool)
    .await?;
    let cursor = rows.last().map_or(after.max(0), |row| row.id);
    let mut events = Vec::with_capacity(rows.len());
    for row in rows {
        let puzzles = if let Some(team_id) = team_id {
            let phase_id = row.phase_id;
            sqlx::query_as!(
                ReleasedPuzzleData,
                "SELECT p.id, p.slug, p.title, p.round_id, r.slug AS round_slug,
                    COALESCE(r.puzzle = p.id, FALSE) AS \"is_round_puzzle!\"
                FROM rb_puzzle p
                JOIN rb_round r ON r.id = p.round_id
                JOIN rb_team_puzzle tp ON tp.puzzle_id = p.id AND tp.team_id = $2
                LEFT JOIN rb_release_event_puzzle rep
                    ON rep.puzzle_id = p.id AND rep.event_id = $3
                LEFT JOIN rb_release_event_puzzle_team rept
                    ON rept.event_id = rep.event_id AND rept.puzzle_id = rep.puzzle_id
                        AND rept.team_id = $2
                WHERE tp.state >= 0
                    AND (($4::SMALLINT = 0 AND p.release_phase_id = $1)
                        OR ($4::SMALLINT = 1 AND rep.event_id IS NOT NULL
                            AND rept.event_id IS NOT NULL
                            AND p.immediate_release_at IS NOT NULL))
                ORDER BY r.sort, r.id, (p.id IS DISTINCT FROM r.puzzle), p.sort, p.id;",
                phase_id,
                team_id,
                row.id,
                row.event_type
            )
            .fetch_all(pool)
            .await?
        } else {
            Vec::new()
        };
        match row.event_type {
            RELEASE_EVENT_PHASE => {
                let phase_id = row.phase_id.ok_or("Release phase event has no phase")?;
                events.push(ReleaseSyncEvent {
                    id: row.id,
                    event_type: "phase_released",
                    occurred_at: row.occurred_at,
                    phase: Some(
                        get_player_phase(pool, phase_id)
                            .await?
                            .ok_or("Release phase not found")?,
                    ),
                    puzzles,
                });
            }
            RELEASE_EVENT_IMMEDIATE_PUZZLES if !puzzles.is_empty() => {
                events.push(ReleaseSyncEvent {
                    id: row.id,
                    event_type: "puzzles_released",
                    occurred_at: row.occurred_at,
                    phase: None,
                    puzzles,
                });
            }
            RELEASE_EVENT_IMMEDIATE_PUZZLES => {}
            _ => return Err("Invalid release event type".into()),
        }
    }
    Ok(ReleaseSyncResult { cursor, events })
}

pub async fn event_cache_targets(
    pool: &DbPool,
    event_id: i64,
    phase_id: Option<i32>,
) -> Result<(Vec<i32>, Vec<i32>), RbInternalError> {
    let puzzles = sqlx::query_scalar!(
        "SELECT p.id FROM rb_puzzle p
        LEFT JOIN rb_release_event_puzzle rep
            ON rep.puzzle_id = p.id AND rep.event_id = $1
        WHERE ($2::INT IS NOT NULL AND p.release_phase_id = $2)
            OR ($2::INT IS NULL AND rep.event_id IS NOT NULL);",
        event_id,
        phase_id
    )
    .fetch_all(pool)
    .await?;
    let rounds = sqlx::query_scalar!(
        "SELECT DISTINCT p.round_id FROM rb_puzzle p
        LEFT JOIN rb_release_event_puzzle rep
            ON rep.puzzle_id = p.id AND rep.event_id = $1
        WHERE ($2::INT IS NOT NULL AND p.release_phase_id = $2)
            OR ($2::INT IS NULL AND rep.event_id IS NOT NULL);",
        event_id,
        phase_id
    )
    .fetch_all(pool)
    .await?;
    Ok((puzzles, rounds))
}
