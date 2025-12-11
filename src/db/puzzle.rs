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

static JUDGE_CACHE: Lazy<DashMap<i32, Arc<Option<Vec<JudgeRule>>>>> = Lazy::new(|| DashMap::new());

pub struct PuzzleSource {
    puzzle_id: Option<i32>,
    game_id: Option<i32>,
    is_intro: bool,
}

impl PuzzleSource {
    pub fn new(puzzle_id: i32) -> Self {
        Self {
            puzzle_id: Some(puzzle_id),
            game_id: None,
            is_intro: false,
        }
    }

    pub fn new_intro(game_id: i32) -> Self {
        Self {
            puzzle_id: None,
            game_id: Some(game_id),
            is_intro: true,
        }
    }
}

pub async fn get_puzzle_game(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    source: &PuzzleSource,
) -> Result<Option<i32>, RbInternalError> {
    if source.is_intro {
        return Ok(source.game_id);
    }

    let mut conn = kv_pool.get().await?;
    let key = format!("puzzle:{}:game", source.puzzle_id.unwrap());

    if let Some(cache) = conn.get(&key).await? {
        return Ok((cache != -1).then(|| cache));
    }

    let result = sqlx::query_scalar!(
        "SELECT r.game_id FROM rb_puzzle p
                JOIN rb_round r ON r.id = p.round_id
                WHERE p.id = $1;",
        source.puzzle_id.unwrap()
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
    source: &PuzzleSource,
) -> Result<RbTeamPuzzleState, RbInternalError> {
    if source.is_intro {
        return Ok(RbTeamPuzzleState::Unlocked);
    }

    let mut conn = kv_pool.get().await?;
    let key = format!("puzzle:{}:team:{team_id}:state", source.puzzle_id.unwrap());

    if let Some(cache) = conn.get::<&str, Option<i16>>(&key).await? {
        return Ok(cache.into());
    }

    let result = sqlx::query_scalar!(
        "SELECT pstate FROM rb_team_puzzle
        WHERE team_id = $1 AND puzzle_id = $2;",
        team_id,
        source.puzzle_id.unwrap()
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

pub async fn check_user_access(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    user_id: i32,
    source: &PuzzleSource,
) -> Result<bool, RbInternalError> {
    let game_id = get_puzzle_game(db_pool, kv_pool, source).await?;
    if game_id.is_none() {
        return Ok(false);
    }
    let game_id = game_id.unwrap();

    // TODO : check game is online & in progress

    let team_id = db::team::get_id_by_user_game(db_pool, kv_pool, user_id, game_id).await?;
    if team_id.is_none() {
        return Ok(false);
    }
    let team_id = team_id.unwrap();

    match source.is_intro {
        true => Ok(true),
        false => Ok(get_puzzle_state(db_pool, kv_pool, team_id, source)
            .await?
            .accessible()),
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
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub ctime_at: OffsetDateTime,
}

pub async fn get_puzzle_show(
    db_pool: &DbPool,
    source: &PuzzleSource,
) -> Result<Option<RbPuzzleShowData>, RbInternalError> {
    let result = match source.is_intro {
        true => {
            sqlx::query_as!(
                RbPuzzleShowData,
                "SELECT p.id, p.title, p.ptype, p.content, p.content_type, p.round_id, p.ctime_at
                FROM rb_game g
                LEFT JOIN rb_puzzle p ON g.intro_puzzle = p.id
                WHERE g.id = $1 AND g.intro_puzzle IS NOT NULL;",
                source.game_id.unwrap()
            )
            .fetch_optional(db_pool)
            .await?
        }
        false => {
            sqlx::query_as!(
                RbPuzzleShowData,
                "SELECT id, title, ptype, content, content_type, round_id, ctime_at
                FROM rb_puzzle
                WHERE id = $1;",
                source.puzzle_id.unwrap()
            )
            .fetch_optional(db_pool)
            .await?
        }
    };

    Ok(result)
}

pub async fn get_judge_rules(
    pool: &DbPool,
    puzzle_id: i32,
) -> Result<Arc<Option<Vec<JudgeRule>>>, RbInternalError> {
    if let Some(c) = JUDGE_CACHE.get(&puzzle_id) {
        return Ok(c.clone());
    }

    let judge_json = sqlx::query_scalar!("SELECT judge FROM rb_puzzle WHERE id = $1;", puzzle_id)
        .fetch_optional(pool)
        .await?;

    let rules = match judge_json {
        Some(s) => Some(game::puzzle::parse_judge(&s)?),
        None => None,
    };

    let rules = Arc::new(rules);
    JUDGE_CACHE.insert(puzzle_id, rules.clone());

    Ok(rules)
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

pub async fn get_team_submissions_by_user(
    pool: &DbPool,
    user_id: i32,
    source: &PuzzleSource,
    page: i64,
) -> Result<Vec<SubmissionUserShowData>, RbInternalError> {
    let result = match source.is_intro {
        true => {
            sqlx::query_as!(
                SubmissionUserShowData,
                "SELECT s.user_id, s.user_answer, s.norm_answer, s.real_answer,
                        s.saction, s.sresult, s.ctime_at
                FROM rb_submission s
                JOIN rb_game g ON g.id = $2 AND s.puzzle_id = g.intro_puzzle
                JOIN rb_team_member m ON m.user_id = $1 AND s.team_id = m.team_id
                ORDER BY s.ctime_at DESC LIMIT 10 OFFSET $3;",
                user_id,
                source.game_id.unwrap(),
                page.saturating_mul(10)
            )
            .fetch_all(pool)
            .await?
        }
        false => {
            sqlx::query_as!(
                SubmissionUserShowData,
                "SELECT s.user_id, s.user_answer, s.norm_answer, s.real_answer,
                        s.saction, s.sresult, s.ctime_at
                FROM rb_submission s
                JOIN rb_team_member m ON m.user_id = $1 AND s.team_id = m.team_id
                WHERE s.puzzle_id = $2
                ORDER BY s.ctime_at DESC LIMIT 10 OFFSET $3;",
                user_id,
                source.puzzle_id.unwrap(),
                page.saturating_mul(10)
            )
            .fetch_all(pool)
            .await?
        }
    };

    Ok(result)
}

pub enum SubmitAnswerResult {
    Ok(JudgeResult),
    Duplicate,
    Invalid,
    NotFound,
}

pub struct SubmitAnswerData {
    user_id: i32,
    source: PuzzleSource,
    answer: String,
}

impl SubmitAnswerData {
    pub fn new(user_id: i32, puzzle_id: i32, answer: &str) -> Self {
        Self {
            user_id,
            source: PuzzleSource::new(puzzle_id),
            answer: answer.to_string(),
        }
    }

    pub fn new_intro(user_id: i32, game_id: i32, answer: &str) -> Self {
        Self {
            user_id,
            source: PuzzleSource::new_intro(game_id),
            answer: answer.to_string(),
        }
    }
}

pub async fn submit_answer(
    pool: &DbPool,
    data: &SubmitAnswerData,
) -> Result<SubmitAnswerResult, RbInternalError> {
    let norm_answer = normalize_answer(&data.answer);
    if norm_answer.is_empty() {
        return Ok(SubmitAnswerResult::Invalid);
    }

    let team_id = match data.source.is_intro {
        true => {
            if data.source.game_id.is_none() {
                return Err("game_id not found".into());
            }
            sqlx::query_scalar!(
                "SELECT m.team_id FROM rb_team_member m
                WHERE m.user_id = $1 AND m.game_id = $2;",
                data.user_id,
                data.source.game_id.unwrap()
            )
            .fetch_optional(pool)
            .await?
        }
        false => {
            if data.source.puzzle_id.is_none() {
                return Err("puzzle_id not found".into());
            }
            sqlx::query_scalar!(
                "SELECT t.id FROM rb_team_member m
                JOIN rb_team t ON t.id = m.team_id
                JOIN rb_team_puzzle p ON p.team_id = t.id
                WHERE m.user_id = $1 AND p.puzzle_id = $2;",
                data.user_id,
                data.source.puzzle_id.unwrap()
            )
            .fetch_optional(pool)
            .await?
        }
    };

    if team_id.is_none() {
        return Ok(SubmitAnswerResult::NotFound);
    }
    let team_id = team_id.unwrap();

    let puzzle_id = match data.source.is_intro {
        true => {
            sqlx::query_scalar!(
                "SELECT intro_puzzle FROM rb_game WHERE id = $1;",
                data.source.game_id.unwrap()
            )
            .fetch_one(pool)
            .await?
        }
        false => data.source.puzzle_id,
    };

    if puzzle_id.is_none() {
        return Ok(SubmitAnswerResult::NotFound);
    }
    let puzzle_id = puzzle_id.unwrap();

    let mut tx = pool.begin().await?;

    let submit_id = sqlx::query_scalar!(
        "INSERT INTO rb_submission (team_id, user_id, puzzle_id, user_answer, norm_answer)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (team_id, puzzle_id, norm_answer) DO NOTHING
        RETURNING id",
        team_id,
        data.user_id,
        puzzle_id,
        &data.answer,
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

    let rules = judge.as_ref().as_ref().unwrap();
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
