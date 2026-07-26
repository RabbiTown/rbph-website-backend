use std::collections::{HashMap, HashSet};

use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgConnection};
use time::OffsetDateTime;

use crate::{
    DbPool,
    error::RbInternalError,
    expr::{self, types::PuzzleStates},
    model::game::RbContentType,
    module::storage::StorageManager,
};

#[derive(Clone, FromRow, Serialize)]
pub struct RbContentBlockAdminData {
    pub id: i32,
    pub puzzle_id: Option<i32>,
    pub round_id: Option<i32>,
    pub sort: i32,
    pub name: String,
    pub content: String,
    pub content_type: i16,
    pub cdn_backend: Option<String>,
    pub cdn_object_key: Option<String>,
    pub cdn_relative_path: Option<String>,
    pub cdn_sha256: Option<String>,
    pub cdn_size: Option<i64>,
    pub visibility_cond: Option<String>,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub ctime_at: OffsetDateTime,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub utime_at: OffsetDateTime,
}

#[derive(Serialize)]
pub struct RbContentBlockShowData {
    pub id: i32,
    pub sort: i32,
    pub content: String,
    pub content_type: RbContentType,
    pub revision: String,
    pub content_url: Option<String>,
}

pub struct ContentBlockArtifact<'a> {
    pub backend: &'a str,
    pub object_key: &'a str,
    pub relative_path: &'a str,
    pub sha256: &'a str,
    pub size: i64,
}

pub struct ContentBlockUpdate<'a> {
    pub name: &'a str,
    pub content: &'a str,
    pub content_type: i16,
    pub visibility_cond: Option<&'a str>,
    pub update_artifact: bool,
    pub artifact: Option<ContentBlockArtifact<'a>>,
}

pub struct ContentBlockArtifactDelete {
    pub backend: String,
    pub object_key: String,
    pub relative_path: String,
}

impl RbContentBlockAdminData {
    pub fn artifact_delete(&self) -> Option<ContentBlockArtifactDelete> {
        Some(ContentBlockArtifactDelete {
            backend: self.cdn_backend.clone()?,
            object_key: self.cdn_object_key.clone()?,
            relative_path: self.cdn_relative_path.clone()?,
        })
    }
}

pub async fn admin_list(
    pool: &DbPool,
    puzzle_id: Option<i32>,
    round_id: Option<i32>,
) -> Result<Vec<RbContentBlockAdminData>, RbInternalError> {
    Ok(sqlx::query_as!(
        RbContentBlockAdminData,
        "SELECT id, puzzle_id, round_id, sort, name, content, content_type,
            cdn_backend, cdn_object_key, cdn_relative_path, cdn_sha256, cdn_size,
            visibility_cond, ctime_at, utime_at
        FROM rb_content_block
        WHERE puzzle_id IS NOT DISTINCT FROM $1 AND round_id IS NOT DISTINCT FROM $2
        ORDER BY sort, id",
        puzzle_id,
        round_id
    )
    .fetch_all(pool)
    .await?)
}

pub async fn admin_get(
    pool: &DbPool,
    id: i32,
) -> Result<Option<RbContentBlockAdminData>, RbInternalError> {
    Ok(sqlx::query_as!(
        RbContentBlockAdminData,
        "SELECT id, puzzle_id, round_id, sort, name, content, content_type,
            cdn_backend, cdn_object_key, cdn_relative_path, cdn_sha256, cdn_size,
            visibility_cond, ctime_at, utime_at
        FROM rb_content_block
        WHERE id = $1",
        id
    )
    .fetch_optional(pool)
    .await?)
}

pub async fn admin_list_for_puzzles(
    pool: &DbPool,
    game_id: i32,
    puzzle_ids: &[i32],
) -> Result<Vec<RbContentBlockAdminData>, RbInternalError> {
    Ok(sqlx::query_as!(
        RbContentBlockAdminData,
        "SELECT cb.id, cb.puzzle_id, cb.round_id, cb.sort, cb.name, cb.content,
            cb.content_type, cb.cdn_backend, cb.cdn_object_key, cb.cdn_relative_path,
            cb.cdn_sha256, cb.cdn_size, cb.visibility_cond, cb.ctime_at, cb.utime_at
        FROM rb_content_block cb
        JOIN rb_puzzle p ON p.id = cb.puzzle_id
        WHERE p.game_id = $1 AND p.id = ANY($2)
        ORDER BY p.id, cb.sort, cb.id",
        game_id,
        puzzle_ids
    )
    .fetch_all(pool)
    .await?)
}

pub async fn admin_puzzles_exist(
    pool: &DbPool,
    game_id: i32,
    puzzle_ids: &[i32],
) -> Result<bool, RbInternalError> {
    let count = sqlx::query_scalar!(
        "SELECT COUNT(*) FROM rb_puzzle WHERE game_id = $1 AND id = ANY($2)",
        game_id,
        puzzle_ids
    )
    .fetch_one(pool)
    .await?
    .unwrap_or(0);
    Ok(count == puzzle_ids.len() as i64)
}

pub async fn admin_create(
    pool: &DbPool,
    puzzle_id: Option<i32>,
    round_id: Option<i32>,
    name: &str,
) -> Result<Option<RbContentBlockAdminData>, RbInternalError> {
    Ok(sqlx::query_as!(
        RbContentBlockAdminData,
        "INSERT INTO rb_content_block (puzzle_id, round_id, sort, name)
        SELECT $1, $2, COALESCE((
            SELECT MAX(cb.sort) + 1 FROM rb_content_block cb
            WHERE cb.puzzle_id IS NOT DISTINCT FROM $1
                AND cb.round_id IS NOT DISTINCT FROM $2
        ), 0), $3
        WHERE (($1::INT IS NOT NULL AND $2::INT IS NULL
                    AND EXISTS (SELECT 1 FROM rb_puzzle WHERE id = $1))
                OR ($2::INT IS NOT NULL AND $1::INT IS NULL
                    AND EXISTS (SELECT 1 FROM rb_round WHERE id = $2)))
        RETURNING id, puzzle_id, round_id, sort, name, content, content_type,
            cdn_backend, cdn_object_key, cdn_relative_path, cdn_sha256, cdn_size,
            visibility_cond, ctime_at, utime_at",
        puzzle_id,
        round_id,
        name
    )
    .fetch_optional(pool)
    .await?)
}

pub async fn admin_update_conn(
    conn: &mut PgConnection,
    id: i32,
    data: ContentBlockUpdate<'_>,
) -> Result<Option<RbContentBlockAdminData>, RbInternalError> {
    let previous_condition = sqlx::query_scalar!(
        "SELECT visibility_cond FROM rb_content_block WHERE id = $1",
        id
    )
    .fetch_optional(&mut *conn)
    .await?;
    let result = sqlx::query_as!(
        RbContentBlockAdminData,
        "UPDATE rb_content_block
        SET name = $2, content = $3, content_type = $4, visibility_cond = $5,
            cdn_backend = CASE WHEN $6 THEN $7 ELSE cdn_backend END,
            cdn_object_key = CASE WHEN $6 THEN $8 ELSE cdn_object_key END,
            cdn_relative_path = CASE WHEN $6 THEN $9 ELSE cdn_relative_path END,
            cdn_sha256 = CASE WHEN $6 THEN $10 ELSE cdn_sha256 END,
            cdn_size = CASE WHEN $6 THEN $11 ELSE cdn_size END,
            utime_at = NOW()
        WHERE id = $1
        RETURNING id, puzzle_id, round_id, sort, name, content, content_type,
            cdn_backend, cdn_object_key, cdn_relative_path, cdn_sha256, cdn_size,
            visibility_cond, ctime_at, utime_at",
        id,
        data.name,
        data.content,
        data.content_type,
        data.visibility_cond,
        data.update_artifact,
        data.artifact.as_ref().map(|artifact| artifact.backend),
        data.artifact.as_ref().map(|artifact| artifact.object_key),
        data.artifact
            .as_ref()
            .map(|artifact| artifact.relative_path),
        data.artifact.as_ref().map(|artifact| artifact.sha256),
        data.artifact.as_ref().map(|artifact| artifact.size),
    )
    .fetch_optional(&mut *conn)
    .await?;
    if result.is_some() && previous_condition.flatten().as_deref() != data.visibility_cond {
        sqlx::query!(
            "UPDATE rb_team SET content_blocks_dirty = TRUE
            WHERE game_id = (
                SELECT COALESCE(p.game_id, r.game_id)
                FROM rb_content_block cb
                LEFT JOIN rb_puzzle p ON p.id = cb.puzzle_id
                LEFT JOIN rb_round r ON r.id = cb.round_id
                WHERE cb.id = $1
            )",
            id
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(result)
}

pub async fn admin_set_artifact_conn(
    conn: &mut PgConnection,
    id: i32,
    artifact: ContentBlockArtifact<'_>,
) -> Result<Option<RbContentBlockAdminData>, RbInternalError> {
    Ok(sqlx::query_as!(
        RbContentBlockAdminData,
        "UPDATE rb_content_block
        SET cdn_backend = $2, cdn_object_key = $3, cdn_relative_path = $4,
            cdn_sha256 = $5, cdn_size = $6, utime_at = NOW()
        WHERE id = $1
        RETURNING id, puzzle_id, round_id, sort, name, content, content_type,
            cdn_backend, cdn_object_key, cdn_relative_path, cdn_sha256, cdn_size,
            visibility_cond, ctime_at, utime_at",
        id,
        artifact.backend,
        artifact.object_key,
        artifact.relative_path,
        artifact.sha256,
        artifact.size,
    )
    .fetch_optional(&mut *conn)
    .await?)
}

pub async fn admin_clear_artifact(
    pool: &DbPool,
    id: i32,
) -> Result<Option<RbContentBlockAdminData>, RbInternalError> {
    Ok(sqlx::query_as!(
        RbContentBlockAdminData,
        "UPDATE rb_content_block
        SET cdn_backend = NULL, cdn_object_key = NULL, cdn_relative_path = NULL,
            cdn_sha256 = NULL, cdn_size = NULL, utime_at = NOW()
        WHERE id = $1
        RETURNING id, puzzle_id, round_id, sort, name, content, content_type,
            cdn_backend, cdn_object_key, cdn_relative_path, cdn_sha256, cdn_size,
            visibility_cond, ctime_at, utime_at",
        id
    )
    .fetch_optional(pool)
    .await?)
}

pub async fn admin_delete(pool: &DbPool, id: i32) -> Result<bool, RbInternalError> {
    Ok(
        sqlx::query!("DELETE FROM rb_content_block WHERE id = $1", id)
            .execute(pool)
            .await?
            .rows_affected()
            > 0,
    )
}

pub async fn admin_reorder(
    pool: &DbPool,
    puzzle_id: Option<i32>,
    round_id: Option<i32>,
    ids: &[i32],
) -> Result<bool, RbInternalError> {
    let current = admin_list(pool, puzzle_id, round_id).await?;
    let current_ids = current.iter().map(|block| block.id).collect::<HashSet<_>>();
    let next_ids = ids.iter().copied().collect::<HashSet<_>>();
    if ids.len() != next_ids.len() || current_ids != next_ids {
        return Ok(false);
    }
    let mut tx = pool.begin().await?;
    for (sort, id) in ids.iter().enumerate() {
        sqlx::query!(
            "UPDATE rb_content_block SET sort = $2, utime_at = NOW() WHERE id = $1",
            id,
            sort as i32
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(true)
}

pub async fn admin_clear_unlocks(pool: &DbPool, id: i32) -> Result<u64, RbInternalError> {
    let mut tx = pool.begin().await?;
    let team_ids = sqlx::query_scalar!(
        "SELECT t.id FROM rb_team t
        WHERE t.id IN (
            SELECT team_id FROM rb_team_content_block_unlock WHERE content_block_id = $1
        ) ORDER BY t.id FOR UPDATE",
        id
    )
    .fetch_all(&mut *tx)
    .await?;
    let deleted = sqlx::query_scalar!(
        "DELETE FROM rb_team_content_block_unlock
        WHERE content_block_id = $1 RETURNING team_id",
        id
    )
    .fetch_all(&mut *tx)
    .await?;
    if !team_ids.is_empty() {
        sqlx::query!(
            "UPDATE rb_team SET content_blocks_dirty = TRUE WHERE id = ANY($1)",
            &team_ids
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(deleted.len() as u64)
}

pub async fn mark_team_dirty_conn(
    conn: &mut PgConnection,
    team_id: i32,
) -> Result<(), RbInternalError> {
    sqlx::query!(
        "UPDATE rb_team SET content_blocks_dirty = TRUE WHERE id = $1",
        team_id
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

struct TeamContentStates {
    solved: HashSet<u32>,
    puzzle_slugs: HashMap<String, u32>,
    round_slugs: HashMap<String, u32>,
    round_puzzles: HashMap<u32, Vec<u32>>,
    triggers: HashSet<(u32, String)>,
    game_started: bool,
}

impl PuzzleStates for TeamContentStates {
    fn is_solved(&self, id: u32) -> bool {
        self.solved.contains(&id)
    }
    fn solved(&self) -> Vec<u32> {
        self.solved.iter().copied().collect()
    }
    fn puzzle_slug(&self, slug: &str) -> Option<u32> {
        self.puzzle_slugs.get(slug).copied()
    }
    fn round_slug(&self, slug: &str) -> Option<u32> {
        self.round_slugs.get(slug).copied()
    }
    fn round_puzzles(&self, id: u32) -> Option<Vec<u32>> {
        self.round_puzzles.get(&id).cloned()
    }
    fn game_started(&self) -> bool {
        self.game_started
    }
    fn is_triggered(&self, id: u32, key: &str) -> bool {
        self.triggers.contains(&(id, key.to_string()))
    }
}

async fn team_states_conn(
    conn: &mut PgConnection,
    team_id: i32,
    game_id: i32,
    game_started: bool,
) -> Result<TeamContentStates, RbInternalError> {
    let solved = sqlx::query_scalar!(
        "SELECT tp.puzzle_id FROM rb_team_puzzle tp JOIN rb_puzzle p ON p.id = tp.puzzle_id
        WHERE tp.team_id = $1 AND p.game_id = $2 AND tp.state >= 1",
        team_id,
        game_id
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .filter_map(|id| id.try_into().ok())
    .collect();
    let puzzles = sqlx::query!(
        "SELECT id, slug, round_id FROM rb_puzzle WHERE game_id = $1",
        game_id
    )
    .fetch_all(&mut *conn)
    .await?;
    let rounds = sqlx::query!("SELECT id, slug FROM rb_round WHERE game_id = $1", game_id)
        .fetch_all(&mut *conn)
        .await?;
    let triggers = sqlx::query!(
        "SELECT tpt.puzzle_id, tpt.trigger_key FROM rb_team_puzzle_trigger tpt
        JOIN rb_puzzle p ON p.id = tpt.puzzle_id WHERE tpt.team_id = $1 AND p.game_id = $2",
        team_id,
        game_id
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .filter_map(|row| {
        row.puzzle_id
            .try_into()
            .ok()
            .map(|id| (id, row.trigger_key))
    })
    .collect();
    let mut puzzle_slugs = HashMap::new();
    let mut round_puzzles: HashMap<u32, Vec<u32>> = HashMap::new();
    for row in puzzles {
        let Ok(id) = row.id.try_into() else {
            continue;
        };
        if let Some(slug) = row.slug {
            puzzle_slugs.insert(slug, id);
        }
        if let Ok(round_id) = row.round_id.try_into() {
            round_puzzles.entry(round_id).or_default().push(id);
        }
    }
    let round_slugs = rounds
        .into_iter()
        .filter_map(|row| Some((row.slug?, row.id.try_into().ok()?)))
        .collect();
    Ok(TeamContentStates {
        solved,
        puzzle_slugs,
        round_slugs,
        round_puzzles,
        triggers,
        game_started,
    })
}

async fn refresh_team_unlocks_if_dirty(
    pool: &DbPool,
    team_id: i32,
    game_id: i32,
) -> Result<(), RbInternalError> {
    let mut tx = pool.begin().await?;
    let team = sqlx::query!(
        "SELECT content_blocks_dirty, is_locked FROM rb_team
        WHERE id = $1 AND game_id = $2 FOR UPDATE",
        team_id,
        game_id
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(team) = team else {
        tx.commit().await?;
        return Ok(());
    };
    if !team.content_blocks_dirty {
        tx.commit().await?;
        return Ok(());
    }

    let blocks = sqlx::query_as!(
        RbContentBlockAdminData,
        "SELECT cb.id, cb.puzzle_id, cb.round_id, cb.sort, cb.name, cb.content,
            cb.content_type, cb.cdn_backend, cb.cdn_object_key, cb.cdn_relative_path,
            cb.cdn_sha256, cb.cdn_size, cb.visibility_cond, cb.ctime_at, cb.utime_at
        FROM rb_content_block cb
        LEFT JOIN rb_puzzle p ON p.id = cb.puzzle_id
        LEFT JOIN rb_round r ON r.id = cb.round_id
        WHERE COALESCE(p.game_id, r.game_id) = $1
            AND cb.visibility_cond IS NOT NULL
            AND (
                (cb.puzzle_id IS NOT NULL AND EXISTS (
                    SELECT 1 FROM rb_team_puzzle tp
                    JOIN rb_puzzle_effective_release release
                        ON release.puzzle_id = tp.puzzle_id
                    WHERE tp.team_id = $2 AND tp.puzzle_id = cb.puzzle_id
                        AND tp.state >= 0 AND release.release_at <= NOW()
                ))
                OR (cb.round_id IS NOT NULL AND EXISTS (
                    SELECT 1 FROM rb_team_puzzle tp
                    JOIN rb_puzzle owner_puzzle ON owner_puzzle.id = tp.puzzle_id
                    JOIN rb_puzzle_effective_release release
                        ON release.puzzle_id = tp.puzzle_id
                    WHERE tp.team_id = $2 AND owner_puzzle.round_id = cb.round_id
                        AND tp.state >= 0 AND release.release_at <= NOW()
                ))
            )
        ORDER BY cb.id",
        game_id,
        team_id
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut unlocked = sqlx::query_scalar!(
        "SELECT content_block_id FROM rb_team_content_block_unlock WHERE team_id = $1",
        team_id
    )
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .collect::<HashSet<_>>();
    let pending = blocks
        .into_iter()
        .filter(|block| !unlocked.contains(&block.id))
        .collect::<Vec<_>>();
    if !pending.is_empty() {
        let states = team_states_conn(&mut tx, team_id, game_id, team.is_locked).await?;
        for block in pending {
            let allowed = block
                .visibility_cond
                .as_deref()
                .and_then(|condition| expr::compile_gate_expr(condition).ok())
                .is_some_and(|cond| expr::ast::eval_compiled(&states, &cond));
            if allowed {
                sqlx::query!(
                    "INSERT INTO rb_team_content_block_unlock (team_id, content_block_id)
                VALUES ($1, $2) ON CONFLICT DO NOTHING",
                    team_id,
                    block.id
                )
                .execute(&mut *tx)
                .await?;
                unlocked.insert(block.id);
            }
        }
    }

    sqlx::query!(
        "UPDATE rb_team SET content_blocks_dirty = FALSE WHERE id = $1",
        team_id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn visible_for_team(
    pool: &DbPool,
    storage: Option<&StorageManager>,
    content_cdn_enabled: bool,
    team_id: i32,
    puzzle_id: Option<i32>,
    round_id: Option<i32>,
    game_id: i32,
) -> Result<Vec<RbContentBlockShowData>, RbInternalError> {
    refresh_team_unlocks_if_dirty(pool, team_id, game_id).await?;
    let blocks = admin_list(pool, puzzle_id, round_id).await?;
    let unlocked = sqlx::query_scalar!(
        "SELECT content_block_id FROM rb_team_content_block_unlock WHERE team_id = $1",
        team_id
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect::<HashSet<_>>();
    let mut visible = Vec::new();
    for block in blocks {
        if block.visibility_cond.is_some() && !unlocked.contains(&block.id) {
            continue;
        }
        let mut hasher = Sha256::new();
        hasher.update(block.content_type.to_le_bytes());
        hasher.update(block.content.as_bytes());
        let content_url = if content_cdn_enabled {
            match (
                storage,
                block.cdn_backend.as_deref(),
                block.cdn_object_key.as_deref(),
                block.cdn_relative_path.as_deref(),
            ) {
                (Some(storage), Some(backend), Some(object_key), Some(relative_path)) => {
                    storage.public_url(backend, object_key, relative_path)
                }
                _ => None,
            }
        } else {
            None
        };
        visible.push(RbContentBlockShowData {
            id: block.id,
            sort: block.sort,
            content: if content_url.is_some() {
                String::new()
            } else {
                block.content
            },
            content_type: block.content_type.into(),
            revision: format!("{:x}", hasher.finalize()),
            content_url,
        });
    }
    Ok(visible)
}
