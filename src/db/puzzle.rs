use std::sync::Arc;

use dashmap::DashMap;
use deadpool_redis::redis::{AsyncCommands, RedisError};
use once_cell::sync::Lazy;
use serde::Serialize;
use sqlx::prelude::FromRow;
use time::OffsetDateTime;

use crate::{
    DbPool, KvPool, db,
    error::RbInternalError,
    game::{
        self,
        puzzle::{JudgeResult, JudgeRule, normalize_answer},
    },
    model::game::{RbContentType, RbJudgeAction, RbPuzzleType, RbTeamPuzzleState},
};

static JUDGE_CACHE: Lazy<DashMap<i32, Arc<Vec<JudgeRule>>>> = Lazy::new(|| DashMap::new());

pub async fn get_puzzle_game(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    puzzle_id: i32,
) -> Result<Option<i32>, RbInternalError> {
    let mut conn = kv_pool.get().await?;
    let key = format!("puzzle:{}:game", puzzle_id);

    if let Some(cache) = conn.get(&key).await? {
        return Ok((cache != -1).then(|| cache));
    }

    let result = sqlx::query_scalar!(
        "SELECT r.game_id FROM rb_puzzle p
        JOIN rb_round r ON r.id = p.round_id
        WHERE p.id = $1;",
        puzzle_id
    )
    .fetch_optional(db_pool)
    .await?;

    let kv_pool = kv_pool.clone();
    tokio::spawn(async move {
        let mut conn = kv_pool.get().await.unwrap();
        let _: Result<(), RedisError> = conn.set_ex(&key, result.unwrap_or(-1), 60 * 60).await;
    });

    Ok(result)
}

pub async fn get_puzzle_state(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    team_id: i32,
    puzzle_id: i32,
) -> Result<RbTeamPuzzleState, RbInternalError> {
    let mut conn = kv_pool.get().await?;
    let key = format!("puzzle:{}:team:{team_id}:state", puzzle_id);

    if let Some(cache) = conn.get::<&str, Option<i16>>(&key).await? {
        return Ok(cache.into());
    }

    let result = sqlx::query_scalar!(
        "SELECT pstate FROM rb_team_puzzle
        WHERE team_id = $1 AND puzzle_id = $2;",
        team_id,
        puzzle_id
    )
    .fetch_optional(db_pool)
    .await?
    .unwrap_or(-1);

    let kv_pool = kv_pool.clone();
    tokio::spawn(async move {
        let mut conn = kv_pool.get().await.unwrap();
        let _: Result<(), RedisError> = conn.set_ex(&key, result, 60 * 60).await;
    });

    Ok(result.into())
}

#[derive(Clone)]
pub struct PuzzleUserInfo {
    pub game_id: i32,
    pub team_id: i32,
}

pub async fn get_puzzle_user_info(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    user_id: i32,
    puzzle_id: i32,
) -> Result<Option<PuzzleUserInfo>, RbInternalError> {
    let game_id = get_puzzle_game(db_pool, kv_pool, puzzle_id).await?;
    if game_id.is_none() {
        return Ok(None);
    }
    let game_id = game_id.unwrap();

    // TODO : check game is online & in progress

    let team_id = db::team::get_id_by_user_game(db_pool, kv_pool, user_id, game_id).await?;
    if team_id.is_none() {
        return Ok(None);
    }
    let team_id = team_id.unwrap();

    let access = get_puzzle_state(db_pool, kv_pool, team_id, puzzle_id)
        .await?
        .accessible();

    match access {
        true => Ok(Some(PuzzleUserInfo { game_id, team_id })),
        false => Ok(None),
    }
}

#[derive(FromRow, Serialize)]
pub struct RbPuzzleShowData {
    pub id: i32,
    pub title: String,
    pub ptype: RbPuzzleType,
    pub content: String,
    pub content_type: RbContentType,
    pub round_id: i32,
}

pub async fn get_puzzle_show(
    db_pool: &DbPool,
    puzzle_id: i32,
) -> Result<Option<RbPuzzleShowData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbPuzzleShowData,
        "SELECT p.id, p.title, p.ptype, p.content, p.content_type, p.round_id
        FROM rb_puzzle p
        WHERE p.id = $1;",
        puzzle_id
    )
    .fetch_optional(db_pool)
    .await?;

    Ok(result)
}

pub async fn get_puzzle_show_str(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    puzzle_id: i32,
) -> Result<Option<String>, RbInternalError> {
    let mut conn = kv_pool.get().await?;
    let key = format!("puzzle:{}:show", puzzle_id);

    if let Some(cache) = conn.get(&key).await? {
        return Ok(Some(cache));
    }

    let result = get_puzzle_show(db_pool, puzzle_id)
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

pub async fn get_puzzle_unlock_time(
    db_pool: &DbPool,
    team_id: i32,
    puzzle_id: i32,
) -> Result<Option<OffsetDateTime>, RbInternalError> {
    let result = sqlx::query_scalar!(
        "SELECT ctime_at
        FROM rb_team_puzzle
        WHERE team_id = $1 AND puzzle_id = $2;",
        team_id,
        puzzle_id
    )
    .fetch_optional(db_pool)
    .await?;

    Ok(result)
}

pub async fn get_puzzle_unlock_time_str(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    team_id: i32,
    puzzle_id: i32,
) -> Result<Option<String>, RbInternalError> {
    let mut conn = kv_pool.get().await?;
    let key = format!("team:{team_id}:puzzle:{}:utime_at", puzzle_id);

    if let Some(cache) = conn.get(&key).await? {
        return Ok(Some(cache));
    }

    let result = get_puzzle_unlock_time(db_pool, team_id, puzzle_id)
        .await?
        .map(|x| crate::serde_helpers::format_offset_datetime(&x));

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

pub async fn get_puzzle_show_str_for_team(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    team_id: i32,
    puzzle_id: i32,
) -> Result<Option<String>, RbInternalError> {
    if let Some(show_str) = get_puzzle_show_str(db_pool, kv_pool, puzzle_id).await? {
        let json = match get_puzzle_unlock_time_str(db_pool, kv_pool, team_id, puzzle_id).await? {
            Some(utime_str) => format!("{{\"data\":{show_str},\"utime_at\":\"{utime_str}\"}}"),
            None => format!("{{\"data\":{show_str}}}"),
        };
        Ok(Some(json))
    } else {
        Ok(None)
    }
}

pub async fn get_judge_rules(
    pool: &DbPool,
    puzzle_id: i32,
) -> Result<Option<Arc<Vec<JudgeRule>>>, RbInternalError> {
    if let Some(c) = JUDGE_CACHE.get(&puzzle_id) {
        return Ok(Some(c.clone()));
    }

    let judge_json = sqlx::query_scalar!("SELECT judge FROM rb_puzzle WHERE id = $1;", puzzle_id)
        .fetch_optional(pool)
        .await?;

    if judge_json.is_none() {
        return Ok(None);
    }

    let rules = game::puzzle::parse_judge(&judge_json.unwrap())?;

    let rules = Arc::new(rules);
    JUDGE_CACHE.insert(puzzle_id, rules.clone());

    Ok(Some(rules))
}

#[derive(FromRow, Serialize)]
pub struct SubmissionUserShowData {
    user_id: i32,
    user_answer: String,
    norm_answer: String,
    saction: RbJudgeAction,
    sresult: Option<String>,
    real_answer: Option<String>,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    ctime_at: OffsetDateTime,
}

pub async fn get_team_submissions(
    pool: &DbPool,
    team_id: i32,
    puzzle_id: i32,
    page: i64,
) -> Result<Vec<SubmissionUserShowData>, RbInternalError> {
    let result = sqlx::query_as!(
        SubmissionUserShowData,
        "SELECT user_id, user_answer, norm_answer, real_answer,
                saction, sresult, ctime_at
        FROM rb_submission
        WHERE puzzle_id = $2 AND team_id = $1
        ORDER BY ctime_at DESC LIMIT 10 OFFSET $3;",
        team_id,
        puzzle_id,
        page.saturating_mul(10)
    )
    .fetch_all(pool)
    .await?;

    Ok(result)
}

pub enum SubmitAnswerResult {
    Ok(JudgeResult),
    Duplicate,
    Invalid,
    NotFound,
}

pub async fn submit_answer(
    pool: &DbPool,
    user_id: i32,
    team_id: i32,
    puzzle_id: i32,
    answer: &str,
) -> Result<SubmitAnswerResult, RbInternalError> {
    let norm_answer = normalize_answer(answer);
    if norm_answer.is_empty() {
        return Ok(SubmitAnswerResult::Invalid);
    }

    let mut tx = pool.begin().await?;

    let submit_id = sqlx::query_scalar!(
        "INSERT INTO rb_submission (team_id, user_id, puzzle_id, user_answer, norm_answer)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (team_id, puzzle_id, norm_answer) DO NOTHING
        RETURNING id",
        team_id,
        user_id,
        puzzle_id,
        answer,
        norm_answer
    )
    .fetch_optional(&mut *tx)
    .await?;

    if submit_id.is_none() {
        return Ok(SubmitAnswerResult::Duplicate);
    }
    let submit_id = submit_id.unwrap();

    let judge = get_judge_rules(pool, puzzle_id).await?;
    if judge.is_none() {
        return Ok(SubmitAnswerResult::NotFound);
    }

    let rules = judge.unwrap();
    let result = game::puzzle::judge_by_rules(&rules, &norm_answer)?;

    let submit_count = sqlx::query_scalar!(
        "UPDATE rb_submission
        SET saction = $1, sresult = $2, real_answer = $3
        WHERE id = $4
        RETURNING (
            SELECT COUNT(*) FROM rb_submission
            WHERE team_id = $5 AND puzzle_id = $6
        );",
        i16::from(result.action),
        result.result,
        result.answer,
        submit_id,
        team_id,
        puzzle_id
    )
    .fetch_one(&mut *tx)
    .await?
    .unwrap();

    tx.commit().await?;

    Ok(SubmitAnswerResult::Ok(result))
}
