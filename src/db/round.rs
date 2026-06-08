use deadpool_redis::redis::{AsyncCommands, RedisError};
use serde::{Deserialize, Serialize};

use crate::{
    AppState, DbPool, KvPool,
    db::{self, game::GameUserInfo, puzzle::RbPuzzleTeamStateShowData},
    error::RbInternalError,
    model::game::{RbContentType, RbTeamPuzzleState},
};

pub async fn get_round_game(
    db_pool: &DbPool,
    round_id: i32,
) -> Result<Option<i32>, RbInternalError> {
    let result = sqlx::query_scalar!(
        "SELECT game_id FROM rb_round
        WHERE id = $1;",
        round_id
    )
    .fetch_optional(db_pool)
    .await?;

    Ok(result)
}

pub async fn get_round_state(
    db_pool: &DbPool,
    team_id: i32,
    round_id: i32,
) -> Result<bool, RbInternalError> {
    let result = sqlx::query_scalar!(
        "SELECT EXISTS (
            SELECT 1 FROM rb_team_puzzle tp
            JOIN rb_puzzle p ON p.id = tp.puzzle_id AND p.round_id = $2
            WHERE tp.team_id = $1 AND tp.state >= 0
        );",
        team_id,
        round_id
    )
    .fetch_one(db_pool)
    .await?
    .unwrap_or(false);

    Ok(result)
}

pub async fn get_round_user_info(
    db_pool: &DbPool,
    user_id: i32,
    round_id: i32,
) -> Result<Option<GameUserInfo>, RbInternalError> {
    let game_id = get_round_game(db_pool, round_id).await?;
    if game_id.is_none() {
        return Ok(None);
    }
    let game_id = game_id.unwrap();

    // TODO : check game is online & in progress

    let team_id = db::team::get_id_by_user_game(db_pool, user_id, game_id).await?;
    if team_id.is_none() {
        return Ok(None);
    }
    let team_id = team_id.unwrap();

    let access = get_round_state(db_pool, team_id, round_id).await?;

    match access {
        true => Ok(Some(GameUserInfo {
            game_id,
            team_id: Some(team_id),
        })),
        false => Ok(None),
    }
}

#[derive(Serialize)]
pub struct RbRoundShowData {
    pub id: i32,
    pub title: String,
    pub content: String,
    pub content_type: RbContentType,
    pub cover: Option<String>,
    pub game_id: i32,
    pub puzzle: Option<i32>,
}

pub async fn get_info_show(
    db_pool: &DbPool,
    round_id: i32,
) -> Result<Option<RbRoundShowData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbRoundShowData,
        "SELECT id, title, content, content_type, cover, game_id, puzzle
        FROM rb_round
        WHERE id = $1",
        round_id
    )
    .fetch_optional(db_pool)
    .await?;

    Ok(result)
}

pub async fn get_info_show_str(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    round_id: i32,
) -> Result<Option<String>, RbInternalError> {
    let mut conn = kv_pool.get().await?;
    let key = format!("round:{round_id}:show");

    if let Some(cache) = conn.get(&key).await? {
        return Ok(Some(cache));
    }

    let result = get_info_show(db_pool, round_id)
        .await?
        .map(|x| serde_json::to_string(&x))
        .transpose()?;

    if result.is_some() {
        let kv_pool = kv_pool.clone();
        let result = result.clone();
        tokio::spawn(async move {
            let mut conn = kv_pool.get().await.unwrap();
            let _: Result<(), RedisError> = conn.set_ex(&key, result, 60 * 60).await;
        });
    }

    Ok(result)
}

#[derive(Serialize)]
pub struct RbPuzzleSimpleData {
    pub id: i32,
    pub title: String,
    pub state: RbTeamPuzzleState,
    pub answer: Option<String>,
}

#[derive(Serialize)]
pub struct RbRoundTeamStateShowData {
    pub puzzles: Vec<RbPuzzleSimpleData>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub puzzle: Option<RbPuzzleTeamStateShowData>,
}

pub async fn get_state_for_team(
    db_pool: &DbPool,
    team_id: i32,
    round_id: i32,
) -> Result<RbRoundTeamStateShowData, RbInternalError> {
    let puzzles = sqlx::query_as!(
        RbPuzzleSimpleData,
        "SELECT p.id, p.title, tp.state AS state,
                CASE WHEN COUNT(s.id) = 1 THEN MAX(s.real_answer) ELSE NULL END AS answer
        FROM rb_puzzle p
        JOIN rb_team_puzzle tp ON tp.puzzle_id = p.id
        LEFT JOIN rb_submission s ON s.puzzle_id = p.id
            AND s.team_id = tp.team_id
            AND (s.saction = 1 OR s.saction = 5)
            AND s.real_answer IS NOT NULL
        WHERE p.round_id = $1 AND tp.team_id = $2 AND tp.state >= 0
            AND p.id IS DISTINCT FROM (SELECT puzzle FROM rb_round WHERE id = $1)
        GROUP BY p.id, p.title, p.sort, tp.state
        ORDER BY p.sort, p.id;",
        round_id,
        team_id
    )
    .fetch_all(db_pool)
    .await?;

    let row = sqlx::query!(
        "SELECT tp.ctime_at AS utime_at, tp.state, tp.cooldown_till,
                tp.max_submit + p.max_submit AS max_submit,
                ARRAY_AGG(s.real_answer) FILTER (WHERE s.real_answer IS NOT NULL) AS answers
        FROM rb_team_puzzle tp
        JOIN rb_round r ON r.id = $2
        JOIN rb_puzzle p ON p.id = tp.puzzle_id
        LEFT JOIN rb_submission s ON s.puzzle_id = tp.puzzle_id
            AND s.team_id = tp.team_id
            AND s.saction = 1
        WHERE tp.team_id = $1 AND tp.puzzle_id = r.puzzle
        GROUP BY tp.ctime_at, tp.state, tp.max_submit, tp.cooldown_till, p.max_submit;",
        team_id,
        round_id
    )
    .fetch_optional(db_pool)
    .await?;

    Ok(RbRoundTeamStateShowData {
        puzzles,
        puzzle: row.map(|r| RbPuzzleTeamStateShowData {
            state: r.state.into(),
            max_submit: r.max_submit,
            answers: r.answers.unwrap_or_default(),
            utime_at: r.utime_at,
            cooldown_till: r.cooldown_till,
        }),
    })
}

pub async fn get_state_for_team_str(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    team_id: i32,
    round_id: i32,
) -> Result<String, RbInternalError> {
    let mut conn = kv_pool.get().await?;
    let key = format!("round:{round_id}:team:{team_id}:full_state");

    if let Some(cache) = conn.get(&key).await? {
        return Ok(cache);
    }

    let result = get_state_for_team(db_pool, team_id, round_id).await?;
    let result = serde_json::to_string(&result)?;

    let kv_pool = kv_pool.clone();
    let result_clone = result.clone();
    tokio::spawn(async move {
        let mut conn = kv_pool.get().await.unwrap();
        let _: Result<(), RedisError> = conn.set_ex(&key, result_clone, 60 * 60).await;
    });

    Ok(result)
}

pub async fn get_info_for_team_str(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    round_id: i32,
    team_id: i32,
) -> Result<Option<String>, RbInternalError> {
    if let Some(show_str) = get_info_show_str(db_pool, kv_pool, round_id).await? {
        let puzzles_str = get_state_for_team_str(db_pool, kv_pool, team_id, round_id).await?;
        Ok(Some(format!(
            "{{\"data\":{show_str},\"state\":{puzzles_str}}}"
        )))
    } else {
        Ok(None)
    }
}

#[derive(Serialize)]
pub struct RbRoundSimpleData {
    pub id: i32,
    pub title: String,
}

pub async fn get_simple_list_for_team(
    app: &AppState,
    game_id: i32,
    team_id: i32,
) -> Result<Vec<RbRoundSimpleData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbRoundSimpleData,
        "SELECT r.id, r.title
        FROM rb_round r
        WHERE r.game_id = $1
        AND EXISTS (
            SELECT 1 FROM rb_puzzle p
            JOIN rb_team_puzzle tp ON tp.puzzle_id = p.id
                AND tp.team_id = $2 AND tp.state >= 0
            WHERE p.round_id = r.id
        )
        ORDER BY r.sort, r.id;",
        game_id,
        team_id
    )
    .fetch_all(&app.db)
    .await?;

    Ok(result)
}

#[derive(Serialize)]
pub struct RbRoundAdminData {
    pub id: i32,
    pub slug: Option<String>,
    pub sort: i32,
    pub title: String,
    pub content: String,
    pub content_type: i16,
    pub cover: Option<String>,
    pub game_id: i32,
    pub puzzle: Option<i32>,
}

#[derive(Deserialize)]
pub struct RbRoundCreateData {
    pub game_id: i32,
    pub slug: Option<String>,
    #[serde(default)]
    pub sort: i32,
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub content_type: i16,
    pub cover: Option<String>,
    pub puzzle: Option<i32>,
}

#[derive(Default, Deserialize)]
pub struct RbRoundUpdateData {
    pub slug: Option<Option<String>>,
    pub sort: Option<i32>,
    pub title: Option<String>,
    pub content: Option<String>,
    pub content_type: Option<i16>,
    pub cover: Option<Option<String>>,
    pub puzzle: Option<Option<i32>>,
}

pub async fn admin_list(
    pool: &DbPool,
    game_id: Option<i32>,
) -> Result<Vec<RbRoundAdminData>, RbInternalError> {
    let result = if let Some(game_id) = game_id {
        sqlx::query_as!(
            RbRoundAdminData,
            "SELECT id, slug, sort, title, content, content_type, cover, game_id, puzzle
        FROM rb_round
        WHERE game_id = $1
        ORDER BY sort, id;",
            game_id
        )
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as!(
            RbRoundAdminData,
            "SELECT id, slug, sort, title, content, content_type, cover, game_id, puzzle
        FROM rb_round
        ORDER BY game_id, sort, id;",
        )
        .fetch_all(pool)
        .await?
    };

    Ok(result)
}

pub async fn admin_get(
    pool: &DbPool,
    round_id: i32,
) -> Result<Option<RbRoundAdminData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbRoundAdminData,
        "SELECT id, slug, sort, title, content, content_type, cover, game_id, puzzle
        FROM rb_round
        WHERE id = $1;",
        round_id
    )
    .fetch_optional(pool)
    .await?;

    Ok(result)
}

pub async fn admin_create(
    pool: &DbPool,
    data: &RbRoundCreateData,
) -> Result<Option<RbRoundAdminData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbRoundAdminData,
        "INSERT INTO rb_round (slug, sort, title, content, content_type, cover, game_id, puzzle)
        SELECT $2, $3, $4, $5, $6, $7, g.id, NULL::INT
        FROM rb_game g
        WHERE g.id = $1 AND $8::INT IS NULL
        RETURNING id, slug, sort, title, content, content_type, cover, game_id, puzzle;",
        data.game_id,
        data.slug,
        data.sort,
        data.title,
        data.content,
        data.content_type,
        data.cover,
        data.puzzle
    )
    .fetch_optional(pool)
    .await?;

    Ok(result)
}

pub async fn admin_update(
    pool: &DbPool,
    round_id: i32,
    data: &RbRoundUpdateData,
) -> Result<Option<RbRoundAdminData>, RbInternalError> {
    let cover_is_set = data.cover.is_some();
    let cover = data.cover.clone().flatten();
    let puzzle_is_set = data.puzzle.is_some();
    let puzzle = data.puzzle.flatten();
    let slug_is_set = data.slug.is_some();
    let slug = data.slug.clone().flatten();

    let result = sqlx::query_as!(
        RbRoundAdminData,
        "UPDATE rb_round r
        SET slug = CASE WHEN $2 THEN $3 ELSE r.slug END,
            sort = COALESCE($4, r.sort),
            title = COALESCE($5, r.title),
            content = COALESCE($6, r.content),
            content_type = COALESCE($7, r.content_type),
            cover = CASE WHEN $8 THEN $9 ELSE r.cover END,
            puzzle = CASE
                WHEN $10 AND $11::INT IS NULL THEN NULL
                WHEN $10 THEN $11::INT
                ELSE r.puzzle
            END
        WHERE r.id = $1
            AND (
                NOT $10 OR $11::INT IS NULL OR EXISTS (
                    SELECT 1
                    FROM rb_puzzle p
                    WHERE p.id = $11::INT AND p.round_id = r.id
                )
            )
        RETURNING id, slug, sort, title, content, content_type, cover, game_id, puzzle;",
        round_id,
        slug_is_set,
        slug,
        data.sort,
        data.title,
        data.content,
        data.content_type,
        cover_is_set,
        cover,
        puzzle_is_set,
        puzzle
    )
    .fetch_optional(pool)
    .await?;

    Ok(result)
}

pub async fn admin_delete(pool: &DbPool, round_id: i32) -> Result<bool, RbInternalError> {
    let result = sqlx::query!(
        "DELETE FROM rb_round
        WHERE id = $1;",
        round_id
    )
    .execute(pool)
    .await?;

    Ok(result.rows_affected() > 0)
}
