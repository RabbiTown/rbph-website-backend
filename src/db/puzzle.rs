use std::{collections::HashSet, sync::Arc};

use dashmap::DashMap;
use deadpool_redis::redis::{AsyncCommands, RedisError};
use once_cell::sync::Lazy;
use serde::Serialize;
use sqlx::prelude::FromRow;
use time::OffsetDateTime;

use crate::{
    DbPool, KvPool,
    db::{self, game::GameUserInfo},
    error::RbInternalError,
    expr::{self, ast::GateExpr, types::PuzzleStates},
    game::{
        self,
        judge::{JudgeResult, JudgeRule, normalize_answer},
    },
    model::game::{RbContentType, RbJudgeAction, RbPuzzleType, RbTeamPuzzleState},
};

static JUDGE_CACHE: Lazy<DashMap<i32, Arc<Vec<JudgeRule>>>> = Lazy::new(DashMap::new);

pub async fn get_puzzle_game(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    puzzle_id: i32,
) -> Result<Option<i32>, RbInternalError> {
    let mut conn = kv_pool.get().await?;
    let key = format!("puzzle:{}:game", puzzle_id);

    if let Some(cache) = conn.get(&key).await? {
        return Ok((cache != -1).then_some(cache));
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
    let key = format!("puzzle:{puzzle_id}:team:{team_id}:state");

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
    .await?;

    if let Some(result) = result {
        let kv_pool = kv_pool.clone();
        tokio::spawn(async move {
            let mut conn = kv_pool.get().await.unwrap();
            let _: Result<(), RedisError> = conn.set_ex(&key, result, 60 * 60).await;
        });
    }

    Ok(result.unwrap_or(-1).into())
}

pub async fn get_puzzle_user_info(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    user_id: i32,
    puzzle_id: i32,
) -> Result<Option<GameUserInfo>, RbInternalError> {
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
        true => Ok(Some(GameUserInfo {
            game_id,
            team_id: Some(team_id),
        })),
        false => Ok(None),
    }
}

#[derive(FromRow, Serialize)]
pub struct RbPuzzleShowRoundData {
    pub id: i32,
    pub title: String,
}

#[derive(FromRow, Serialize)]
pub struct RbPuzzleShowAnnouncementData {
    pub id: i32,
    pub title: String,
    pub content: String,
    pub content_type: RbContentType,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub utime_at: OffsetDateTime,
}

#[derive(FromRow, Serialize)]
pub struct RbPuzzleShowData {
    pub id: i32,
    pub title: String,
    pub ptype: RbPuzzleType,
    pub content: String,
    pub content_type: RbContentType,
    pub round: RbPuzzleShowRoundData,
    pub game_id: i32,
    pub announcements: Vec<RbPuzzleShowAnnouncementData>,
}

pub async fn get_puzzle_show(
    db_pool: &DbPool,
    puzzle_id: i32,
) -> Result<Option<RbPuzzleShowData>, RbInternalError> {
    let result = sqlx::query!(
        "SELECT p.id, p.title, p.ptype, p.content, p.content_type,
                p.round_id, r.title AS round_title, r.game_id
        FROM rb_puzzle p
        JOIN rb_round r ON r.id = p.round_id AND r.puzzle != p.id
        WHERE p.id = $1;",
        puzzle_id
    )
    .fetch_optional(db_pool)
    .await?;

    let anmts = sqlx::query_as!(
        RbPuzzleShowAnnouncementData,
        "SELECT id, title, content, content_type, utime_at
        FROM rb_announcement
        WHERE puzzle_id = $1;",
        puzzle_id
    )
    .fetch_all(db_pool)
    .await?;

    Ok(result.map(|x| RbPuzzleShowData {
        id: x.id,
        title: x.title,
        ptype: x.ptype.into(),
        content: x.content,
        content_type: x.content_type.into(),
        round: RbPuzzleShowRoundData {
            id: x.round_id,
            title: x.round_title,
        },
        game_id: x.game_id,
        announcements: anmts,
    }))
}

pub async fn get_puzzle_show_str(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    puzzle_id: i32,
) -> Result<Option<String>, RbInternalError> {
    let mut conn = kv_pool.get().await?;
    let key = format!("puzzle:{puzzle_id}:show");

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

#[derive(FromRow, Serialize)]
pub struct RbPuzzleTeamStateShowData {
    pub state: RbTeamPuzzleState,
    pub answers: Vec<String>,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub utime_at: OffsetDateTime,
}

pub async fn get_puzzle_team_state(
    db_pool: &DbPool,
    team_id: i32,
    puzzle_id: i32,
) -> Result<Option<RbPuzzleTeamStateShowData>, RbInternalError> {
    let rows = sqlx::query!(
        "SELECT tp.ctime_at AS utime_at, tp.pstate, s.real_answer
        FROM rb_team_puzzle tp
        LEFT JOIN rb_submission s ON s.puzzle_id = tp.puzzle_id
            AND s.team_id = tp.team_id
            AND s.saction = 1
            AND s.real_answer IS NOT NULL
        WHERE tp.team_id = $1 AND tp.puzzle_id = $2;",
        team_id,
        puzzle_id
    )
    .fetch_all(db_pool)
    .await?;

    if rows.is_empty() {
        return Ok(None);
    }

    let utime_at = rows.first().unwrap().utime_at;
    let state = rows.first().unwrap().pstate;
    let answers = rows.into_iter().filter_map(|r| r.real_answer).collect();

    Ok(Some(RbPuzzleTeamStateShowData {
        utime_at,
        state: state.into(),
        answers,
    }))
}

pub async fn get_puzzle_team_state_str(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    team_id: i32,
    puzzle_id: i32,
) -> Result<Option<String>, RbInternalError> {
    let mut conn = kv_pool.get().await?;
    let key = format!("puzzle:{puzzle_id}:team:{team_id}:full_state");

    if let Some(cache) = conn.get(&key).await? {
        return Ok(Some(cache));
    }

    let result = get_puzzle_team_state(db_pool, team_id, puzzle_id)
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

pub async fn get_puzzle_show_str_for_team(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    team_id: i32,
    puzzle_id: i32,
) -> Result<Option<String>, RbInternalError> {
    if let Some(show_str) = get_puzzle_show_str(db_pool, kv_pool, puzzle_id).await? {
        let json = match get_puzzle_team_state_str(db_pool, kv_pool, team_id, puzzle_id).await? {
            Some(state_str) => format!("{{\"data\":{show_str},\"state\":{state_str}}}"),
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

    let rules = game::judge::parse_judge(&judge_json.unwrap())?;

    let rules = Arc::new(rules);
    JUDGE_CACHE.insert(puzzle_id, rules.clone());

    Ok(Some(rules))
}

static UNLOCK_COND_CACHE: Lazy<DashMap<i32, Arc<Vec<(i32, GateExpr)>>>> = Lazy::new(DashMap::new);

pub async fn get_unlock_conds_by_game(
    pool: &DbPool,
    game_id: i32,
) -> Result<Arc<Vec<(i32, GateExpr)>>, RbInternalError> {
    if let Some(c) = UNLOCK_COND_CACHE.get(&game_id) {
        return Ok(c.clone());
    }

    let raw_exprs = sqlx::query!(
        "SELECT p.id, p.unlock_cond
        FROM rb_puzzle p
        JOIN rb_round r ON r.id = p.round_id
        JOIN rb_game g ON g.id = r.game_id
        WHERE g.id = $1;",
        game_id
    )
    .fetch_all(pool)
    .await?;

    let exprs: Vec<(i32, GateExpr)> = raw_exprs
        .iter()
        .filter_map(|r| {
            if r.unlock_cond == "default" {
                return None;
            }
            match expr::compile_gate_expr(&r.unlock_cond) {
                Ok(expr) => Some((r.id, expr)),
                Err(e) => {
                    log::warn!("Failed to parse unlock_cond for puzzle {}: {}", r.id, e);
                    None
                }
            }
        })
        .collect();

    let arc_expr = Arc::new(exprs);
    UNLOCK_COND_CACHE.insert(game_id, arc_expr.clone());

    Ok(arc_expr)
}

#[derive(FromRow, Serialize)]
pub struct SubmissionUserShowData {
    user_name: String,
    user_answer: String,
    norm_answer: String,
    saction: RbJudgeAction,
    sresult: Option<String>,
    real_answer: Option<String>,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    ctime_at: OffsetDateTime,
}

#[derive(FromRow, Serialize)]
pub struct SubmissionPageData {
    data: Vec<SubmissionUserShowData>,
    total: i64,
}

pub async fn get_team_submissions(
    pool: &DbPool,
    team_id: i32,
    puzzle_id: i32,
    page: i64,
    only_ok: bool,
) -> Result<SubmissionPageData, RbInternalError> {
    let rows = sqlx::query!(
        "SELECT u.nickname AS user_name, s.user_answer, s.norm_answer,
                s.real_answer, s.saction, s.sresult, s.ctime_at,
                COUNT(*) OVER() AS total
        FROM rb_submission s
        JOIN rb_user u ON u.id = s.user_id
        WHERE s.puzzle_id = $2 AND s.team_id = $1 AND (NOT $4 OR s.saction > 0)
        ORDER BY ctime_at DESC LIMIT 10 OFFSET $3;",
        team_id,
        puzzle_id,
        page.saturating_mul(10),
        only_ok
    )
    .fetch_all(pool)
    .await?;

    let total = rows.first().and_then(|x| x.total).unwrap_or(0);

    let data = rows
        .into_iter()
        .map(|x| SubmissionUserShowData {
            user_name: x.user_name,
            user_answer: x.user_answer,
            norm_answer: x.norm_answer,
            saction: x.saction.into(),
            sresult: x.sresult,
            real_answer: x.real_answer,
            ctime_at: x.ctime_at,
        })
        .collect();

    Ok(SubmissionPageData { data, total })
}

pub enum SubmitAnswerResult {
    Ok(JudgeResult),
    Duplicate,
    Invalid,
    NotFound,
}

pub async fn submit_answer(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    user_id: i32,
    team_id: i32,
    puzzle_id: i32,
    answer: &str,
) -> Result<SubmitAnswerResult, RbInternalError> {
    let norm_answer = normalize_answer(answer);
    if norm_answer.is_empty() {
        return Ok(SubmitAnswerResult::Invalid);
    }

    let mut tx = db_pool.begin().await?;

    let submit_id = sqlx::query_scalar!(
        "INSERT INTO rb_submission (team_id, user_id, puzzle_id, user_answer, norm_answer)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT DO NOTHING
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

    let judge = get_judge_rules(db_pool, puzzle_id).await?;
    if judge.is_none() {
        return Ok(SubmitAnswerResult::NotFound);
    }

    let rules = judge.unwrap();
    let result = game::judge::judge_by_rules(&rules, &norm_answer)?;

    let submit_count = sqlx::query_scalar!(
        "UPDATE rb_submission
        SET saction = $1, sresult = $2, real_answer = $3, ignored = $7
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
        puzzle_id,
        result.action == RbJudgeAction::Error
    )
    .fetch_one(&mut *tx)
    .await?
    .unwrap();

    match result.action {
        RbJudgeAction::Correct => {
            let result = sqlx::query!(
                "UPDATE rb_team_puzzle SET pstate = 1, solve_at = NOW()
                WHERE team_id = $1 AND puzzle_id = $2 AND pstate = 0",
                team_id,
                puzzle_id
            )
            .execute(db_pool)
            .await?;

            if result.rows_affected() > 0 {
                unlock_new_puzzles(db_pool, team_id).await?
            }
        }
        RbJudgeAction::StartGame => {
            let result = sqlx::query!(
                "UPDATE rb_team SET tstate = 1
                WHERE id = $1 AND tstate = 0",
                team_id
            )
            .execute(db_pool)
            .await?;

            if result.rows_affected() > 0 {
                sqlx::query!(
                    "INSERT INTO rb_team_currency (team_id, currency_id)
                    SELECT t.id AS team_id, c.id AS currency_id
                    FROM rb_team t
                    JOIN rb_currency c ON c.game_id = t.game_id
                    WHERE t.id = $1
                    ON CONFLICT (team_id, currency_id) DO NOTHING;",
                    team_id
                )
                .execute(db_pool)
                .await?;

                unlock_new_puzzles(db_pool, team_id).await?
            }
        }
        _ => {}
    }

    tx.commit().await?;

    if result.action.side_effect() {
        db::cache::invalidate_team_puzzle(db_pool, kv_pool, team_id, puzzle_id).await?;
    }

    Ok(SubmitAnswerResult::Ok(result))
}

struct RbPuzzleStates {
    unlocked: HashSet<i32>,
    game_started: bool,
}

impl PuzzleStates for RbPuzzleStates {
    fn is_completed(&self, id: expr::types::PuzzleId) -> bool {
        self.unlocked.contains(&id.try_into().unwrap_or(i32::MAX))
    }

    fn completed_count(&self) -> expr::types::CountSize {
        self.unlocked.len()
    }

    fn completed(&self) -> Vec<expr::types::PuzzleId> {
        self.unlocked
            .iter()
            .map(|&x| x.try_into().unwrap_or(0))
            .collect()
    }

    fn game_started(&self) -> bool {
        self.game_started
    }
}

pub async fn unlock_new_puzzles(db_pool: &DbPool, team_id: i32) -> Result<(), RbInternalError> {
    let info = sqlx::query!(
        "SELECT t.game_id, t.tstate, tp.puzzle_id AS \"puzzle_id?\"
        FROM rb_team t
        LEFT JOIN rb_team_puzzle tp ON tp.team_id = t.id AND tp.pstate >= 1
        WHERE t.id = $1;",
        team_id
    )
    .fetch_all(db_pool)
    .await?;

    // dont know if possible but we just protect from it
    if info.is_empty() {
        return Ok(());
    }

    let unlocked = info.iter().filter_map(|r| r.puzzle_id).collect();
    let state = RbPuzzleStates {
        unlocked,
        game_started: info[0].tstate > 0,
    };

    let conds = get_unlock_conds_by_game(db_pool, info[0].game_id).await?;

    let mut unlocks: Vec<i32> = Vec::new();

    for cond in conds.iter() {
        if !state.is_completed(cond.0.try_into().unwrap_or(0))
            && expr::ast::eval_compiled(&state, &cond.1)
        {
            unlocks.push(cond.0);
        }
    }

    if !unlocks.is_empty() {
        sqlx::query!(
            "INSERT INTO rb_team_puzzle (team_id, puzzle_id, pstate)
            SELECT $1, UNNEST($2::int[]), 0
            ON CONFLICT DO NOTHING;",
            team_id,
            &unlocks
        )
        .execute(db_pool)
        .await?;
    }

    Ok(())
}

#[derive(FromRow, Serialize)]
pub struct RbHintShowData {
    pub id: i32,
    pub title: String,
    pub cooldown: i32,
    pub cost_id: Option<i32>,
    pub cost_amount: i32,
}

pub async fn get_hints_show(
    db_pool: &DbPool,
    puzzle_id: i32,
) -> Result<Vec<RbHintShowData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbHintShowData,
        "SELECT h.id, h.title, h.cooldown, h.cost_id, h.cost_amount
        FROM rb_hint h
        JOIN rb_puzzle p ON p.id = h.puzzle_id
        WHERE p.id = $1
        ORDER BY h.sort, h.id;",
        puzzle_id
    )
    .fetch_all(db_pool)
    .await?;

    Ok(result)
}

pub async fn get_hints_show_str(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    puzzle_id: i32,
) -> Result<String, RbInternalError> {
    let mut conn = kv_pool.get().await?;
    let key = format!("puzzle:{puzzle_id}:hints");

    if let Some(cache) = conn.get(&key).await? {
        return Ok(cache);
    }

    let result = get_hints_show(db_pool, puzzle_id).await?;
    let result = serde_json::to_string(&result)?;

    let kv_pool = kv_pool.clone();
    let result_clone = result.clone();
    tokio::spawn(async move {
        let mut conn = kv_pool.get().await.unwrap();
        let _: Result<(), RedisError> = conn.set_ex(&key, result_clone, 60 * 60).await;
    });

    Ok(result)
}

#[derive(FromRow, Serialize)]
pub struct RbHintTeamStateShowData {
    pub id: i32,
    pub content: String,
    pub content_type: RbContentType,
}

pub async fn get_hints_team_state(
    db_pool: &DbPool,
    team_id: i32,
    puzzle_id: i32,
) -> Result<Vec<RbHintTeamStateShowData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbHintTeamStateShowData,
        "SELECT h.id, h.content, h.content_type
        FROM rb_hint h
        JOIN rb_team_hint th ON th.hint_id = h.id
        WHERE th.team_id = $1 AND h.puzzle_id = $2 AND th.unlocked;",
        team_id,
        puzzle_id
    )
    .fetch_all(db_pool)
    .await?;

    Ok(result)
}

pub async fn get_hints_team_state_str(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    team_id: i32,
    puzzle_id: i32,
) -> Result<String, RbInternalError> {
    let mut conn = kv_pool.get().await?;
    let key = format!("puzzle:{puzzle_id}:team:{team_id}:hints");

    if let Some(cache) = conn.get(&key).await? {
        return Ok(cache);
    }

    let result = get_hints_team_state(db_pool, team_id, puzzle_id).await?;
    let result = serde_json::to_string(&result)?;

    let kv_pool = kv_pool.clone();
    let result_clone = result.clone();
    tokio::spawn(async move {
        let mut conn = kv_pool.get().await.unwrap();
        let _: Result<(), RedisError> = conn.set_ex(&key, result_clone, 60 * 60).await;
    });

    Ok(result)
}

pub async fn get_hints_show_str_for_team(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    team_id: i32,
    puzzle_id: i32,
) -> Result<String, RbInternalError> {
    let show_str = get_hints_show_str(db_pool, kv_pool, puzzle_id).await?;
    let state_str = get_hints_team_state_str(db_pool, kv_pool, team_id, puzzle_id).await?;
    Ok(format!("{{\"data\":{show_str},\"state\":{state_str}}}"))
}

pub enum PurchaseHintResult {
    Insufficient,
    Unavailable,
    Ok(RbHintTeamStateShowData),
}

pub async fn purchase_hint(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    user_id: i32,
    hint_id: i32,
) -> Result<PurchaseHintResult, RbInternalError> {
    let info = sqlx::query!(
        "SELECT r.game_id, tm.team_id, h.puzzle_id, h.cost_id, h.cost_amount
        FROM rb_hint h
        JOIN rb_puzzle p ON p.id = h.puzzle_id
        JOIN rb_round r ON r.id = p.round_id
        JOIN rb_team_member tm ON tm.game_id = r.game_id
        JOIN rb_team_puzzle tp ON tp.puzzle_id = p.id AND tp.team_id = tm.team_id
        LEFT JOIN rb_team_hint th ON th.hint_id = h.id AND th.team_id = tm.team_id
        WHERE tm.user_id = $1 AND h.id = $2 AND tp.pstate >= 0
            AND NOT COALESCE(th.unlocked, FALSE)
            AND EXTRACT(EPOCH FROM (NOW() - tp.ctime_at)) >= h.cooldown;",
        user_id,
        hint_id
    )
    .fetch_optional(db_pool)
    .await?;

    if info.is_none() {
        return Ok(PurchaseHintResult::Unavailable);
    }
    let info = info.unwrap();

    let mut tx = db_pool.begin().await?;

    if info.cost_id.is_some() {
        let result = sqlx::query!(
            "UPDATE rb_team_currency tc
            SET utime_at = NOW(), amount = LEAST(
                tc.amount + (EXTRACT(EPOCH FROM (NOW() - tc.utime_at))::INT / 60) * (c.growth + tc.growth),
                c.max_amount
            ) - $3
            FROM rb_currency c
            WHERE tc.currency_id = c.id AND tc.team_id = $1 AND c.id = $2
                AND LEAST(
                    tc.amount + (EXTRACT(EPOCH FROM (NOW() - tc.utime_at))::INT / 60) * (c.growth + tc.growth),
                    c.max_amount
                ) >= $3;",
            info.team_id, info.cost_id, info.cost_amount
        )
        .execute(&mut *tx)
        .await?;

        if result.rows_affected() == 0 {
            return Ok(PurchaseHintResult::Insufficient);
        }
    }

    let result = sqlx::query_as!(
        RbHintTeamStateShowData,
        "
        WITH upserted AS (
            INSERT INTO rb_team_hint (team_id, hint_id, unlocked)
            VALUES ($1, $2, TRUE)
            ON CONFLICT (team_id, hint_id)
            DO UPDATE SET unlocked = TRUE
            RETURNING hint_id
        )
        SELECT h.id, h.content, h.content_type
        FROM rb_hint h
        JOIN upserted u ON h.id = u.hint_id",
        info.team_id,
        hint_id
    )
    .fetch_one(db_pool)
    .await?;

    db::cache::invalidate_team_hints(kv_pool, info.team_id, info.puzzle_id).await?;

    tx.commit().await?;
    Ok(PurchaseHintResult::Ok(result))
}
