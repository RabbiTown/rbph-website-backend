use std::collections::{HashMap, HashSet};

use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgConnection, Postgres, Transaction};
use time::OffsetDateTime;

use crate::{
    DbPool,
    error::RbInternalError,
    expr::{self, types::PuzzleStates},
    model::game::RbContentType,
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
    pub visibility_cond: String,
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

pub async fn admin_list(
    pool: &DbPool,
    puzzle_id: Option<i32>,
    round_id: Option<i32>,
) -> Result<Vec<RbContentBlockAdminData>, RbInternalError> {
    Ok(sqlx::query_as::<_, RbContentBlockAdminData>(
        "SELECT id, puzzle_id, round_id, sort, name, content, content_type,
            visibility_cond, ctime_at, utime_at
        FROM rb_content_block
        WHERE puzzle_id IS NOT DISTINCT FROM $1 AND round_id IS NOT DISTINCT FROM $2
        ORDER BY sort, id",
    )
    .bind(puzzle_id)
    .bind(round_id)
    .fetch_all(pool)
    .await?)
}

pub async fn admin_create(
    pool: &DbPool,
    puzzle_id: Option<i32>,
    round_id: Option<i32>,
    name: &str,
) -> Result<Option<RbContentBlockAdminData>, RbInternalError> {
    Ok(sqlx::query_as::<_, RbContentBlockAdminData>(
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
            visibility_cond, ctime_at, utime_at",
    )
    .bind(puzzle_id)
    .bind(round_id)
    .bind(name)
    .fetch_optional(pool)
    .await?)
}

pub async fn admin_update(
    tx: &mut Transaction<'_, Postgres>,
    id: i32,
    name: &str,
    content: &str,
    content_type: i16,
    visibility_cond: &str,
) -> Result<Option<RbContentBlockAdminData>, RbInternalError> {
    let previous_condition = sqlx::query_scalar::<_, String>(
        "SELECT visibility_cond FROM rb_content_block WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    let result = sqlx::query_as::<_, RbContentBlockAdminData>(
        "UPDATE rb_content_block
        SET name = $2, content = $3, content_type = $4, visibility_cond = $5,
            utime_at = NOW()
        WHERE id = $1
        RETURNING id, puzzle_id, round_id, sort, name, content, content_type,
            visibility_cond, ctime_at, utime_at",
    )
    .bind(id)
    .bind(name)
    .bind(content)
    .bind(content_type)
    .bind(visibility_cond)
    .fetch_optional(&mut **tx)
    .await?;
    if result.is_some() && previous_condition.as_deref() != Some(visibility_cond) {
        sqlx::query(
            "UPDATE rb_team SET content_blocks_dirty = TRUE
            WHERE game_id = (
                SELECT COALESCE(p.game_id, r.game_id)
                FROM rb_content_block cb
                LEFT JOIN rb_puzzle p ON p.id = cb.puzzle_id
                LEFT JOIN rb_round r ON r.id = cb.round_id
                WHERE cb.id = $1
            )",
        )
        .bind(id)
        .execute(&mut **tx)
        .await?;
    }
    Ok(result)
}

pub async fn admin_delete(pool: &DbPool, id: i32) -> Result<bool, RbInternalError> {
    Ok(sqlx::query("DELETE FROM rb_content_block WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?
        .rows_affected()
        > 0)
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
        sqlx::query("UPDATE rb_content_block SET sort = $2, utime_at = NOW() WHERE id = $1")
            .bind(id)
            .bind(sort as i32)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(true)
}

pub async fn admin_clear_unlocks(pool: &DbPool, id: i32) -> Result<u64, RbInternalError> {
    let mut tx = pool.begin().await?;
    let team_ids = sqlx::query_scalar::<_, i32>(
        "SELECT t.id FROM rb_team t
        WHERE t.id IN (
            SELECT team_id FROM rb_team_content_block_unlock WHERE content_block_id = $1
        ) ORDER BY t.id FOR UPDATE",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    let deleted = sqlx::query_scalar::<_, i32>(
        "DELETE FROM rb_team_content_block_unlock
        WHERE content_block_id = $1 RETURNING team_id",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    if !team_ids.is_empty() {
        sqlx::query("UPDATE rb_team SET content_blocks_dirty = TRUE WHERE id = ANY($1)")
            .bind(&team_ids)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(deleted.len() as u64)
}

pub async fn mark_team_dirty_tx(
    tx: &mut Transaction<'_, Postgres>,
    team_id: i32,
) -> Result<(), RbInternalError> {
    sqlx::query("UPDATE rb_team SET content_blocks_dirty = TRUE WHERE id = $1")
        .bind(team_id)
        .execute(&mut **tx)
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

async fn team_states(
    conn: &mut PgConnection,
    team_id: i32,
    game_id: i32,
    game_started: bool,
) -> Result<TeamContentStates, RbInternalError> {
    let solved = sqlx::query_scalar::<_, i32>(
        "SELECT tp.puzzle_id FROM rb_team_puzzle tp JOIN rb_puzzle p ON p.id = tp.puzzle_id
        WHERE tp.team_id = $1 AND p.game_id = $2 AND tp.state >= 1",
    )
    .bind(team_id)
    .bind(game_id)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .filter_map(|id| id.try_into().ok())
    .collect();
    let puzzles = sqlx::query_as::<_, (i32, Option<String>, i32)>(
        "SELECT id, slug, round_id FROM rb_puzzle WHERE game_id = $1",
    )
    .bind(game_id)
    .fetch_all(&mut *conn)
    .await?;
    let rounds = sqlx::query_as::<_, (i32, Option<String>)>(
        "SELECT id, slug FROM rb_round WHERE game_id = $1",
    )
    .bind(game_id)
    .fetch_all(&mut *conn)
    .await?;
    let triggers = sqlx::query_as::<_, (i32, String)>(
        "SELECT tpt.puzzle_id, tpt.trigger_key FROM rb_team_puzzle_trigger tpt
        JOIN rb_puzzle p ON p.id = tpt.puzzle_id WHERE tpt.team_id = $1 AND p.game_id = $2",
    )
    .bind(team_id)
    .bind(game_id)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .filter_map(|(id, key)| id.try_into().ok().map(|id| (id, key)))
    .collect();
    let mut puzzle_slugs = HashMap::new();
    let mut round_puzzles: HashMap<u32, Vec<u32>> = HashMap::new();
    for (id, slug, round_id) in puzzles {
        let Ok(id) = id.try_into() else { continue };
        if let Some(slug) = slug {
            puzzle_slugs.insert(slug, id);
        }
        if let Ok(round_id) = round_id.try_into() {
            round_puzzles.entry(round_id).or_default().push(id);
        }
    }
    let round_slugs = rounds
        .into_iter()
        .filter_map(|(id, slug)| Some((slug?, id.try_into().ok()?)))
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
    let team = sqlx::query_as::<_, (bool, bool)>(
        "SELECT content_blocks_dirty, is_locked FROM rb_team
        WHERE id = $1 AND game_id = $2 FOR UPDATE",
    )
    .bind(team_id)
    .bind(game_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((dirty, game_started)) = team else {
        tx.commit().await?;
        return Ok(());
    };
    if !dirty {
        tx.commit().await?;
        return Ok(());
    }

    let blocks = sqlx::query_as::<_, RbContentBlockAdminData>(
        "SELECT cb.id, cb.puzzle_id, cb.round_id, cb.sort, cb.name, cb.content,
            cb.content_type, cb.visibility_cond, cb.ctime_at, cb.utime_at
        FROM rb_content_block cb
        LEFT JOIN rb_puzzle p ON p.id = cb.puzzle_id
        LEFT JOIN rb_round r ON r.id = cb.round_id
        WHERE COALESCE(p.game_id, r.game_id) = $1
            AND cb.visibility_cond != 'default'
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
    )
    .bind(game_id)
    .bind(team_id)
    .fetch_all(&mut *tx)
    .await?;
    let mut unlocked = sqlx::query_scalar::<_, i32>(
        "SELECT content_block_id FROM rb_team_content_block_unlock WHERE team_id = $1",
    )
    .bind(team_id)
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .collect::<HashSet<_>>();
    let pending = blocks
        .into_iter()
        .filter(|block| !unlocked.contains(&block.id))
        .collect::<Vec<_>>();
    if !pending.is_empty() {
        let states = team_states(&mut tx, team_id, game_id, game_started).await?;
        for block in pending {
            let allowed = expr::compile_gate_expr(&block.visibility_cond)
                .ok()
                .is_some_and(|cond| expr::ast::eval_compiled(&states, &cond));
            if allowed {
                sqlx::query(
                    "INSERT INTO rb_team_content_block_unlock (team_id, content_block_id)
                VALUES ($1, $2) ON CONFLICT DO NOTHING",
                )
                .bind(team_id)
                .bind(block.id)
                .execute(&mut *tx)
                .await?;
                unlocked.insert(block.id);
            }
        }
    }

    sqlx::query("UPDATE rb_team SET content_blocks_dirty = FALSE WHERE id = $1")
        .bind(team_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn visible_for_team(
    pool: &DbPool,
    team_id: i32,
    puzzle_id: Option<i32>,
    round_id: Option<i32>,
    game_id: i32,
) -> Result<Vec<RbContentBlockShowData>, RbInternalError> {
    refresh_team_unlocks_if_dirty(pool, team_id, game_id).await?;
    let blocks = admin_list(pool, puzzle_id, round_id).await?;
    let unlocked = sqlx::query_scalar::<_, i32>(
        "SELECT content_block_id FROM rb_team_content_block_unlock WHERE team_id = $1",
    )
    .bind(team_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect::<HashSet<_>>();
    let mut visible = Vec::new();
    for block in blocks {
        if block.visibility_cond != "default" && !unlocked.contains(&block.id) {
            continue;
        }
        let mut hasher = Sha256::new();
        hasher.update(block.content_type.to_le_bytes());
        hasher.update(block.content.as_bytes());
        visible.push(RbContentBlockShowData {
            id: block.id,
            sort: block.sort,
            content: block.content,
            content_type: block.content_type.into(),
            revision: format!("{:x}", hasher.finalize()),
            content_url: None,
        });
    }
    Ok(visible)
}
