use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use dashmap::DashMap;
use deadpool_redis::redis::{AsyncCommands, RedisError};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::prelude::FromRow;
use time::OffsetDateTime;

use crate::{
    AppState, DbPool, KvPool,
    db::{self, game::GameUserInfo},
    error::RbInternalError,
    expr::{self, ast::GateExpr, types::PuzzleStates},
    extractor::auth::AuthUser,
    game::{
        self,
        judge::{JudgeResult, JudgeRule, judge_by_rules, normalize_answer},
    },
    model::game::{
        RbContentType, RbJudgeAction, RbPuzzlePenaltyType, RbPuzzleType, RbTeamPuzzleState,
    },
    model::user::RbUserRole,
};

static JUDGE_CACHE: Lazy<DashMap<i32, Arc<Vec<JudgeRule>>>> = Lazy::new(DashMap::new);

pub async fn get_puzzle_game(
    db_pool: &DbPool,
    puzzle_id: i32,
) -> Result<Option<i32>, RbInternalError> {
    let result = sqlx::query_scalar!(
        "SELECT r.game_id FROM rb_puzzle p
        JOIN rb_round r ON r.id = p.round_id
        WHERE p.id = $1;",
        puzzle_id
    )
    .fetch_optional(db_pool)
    .await?;

    Ok(result)
}

#[derive(FromRow)]
pub struct PuzzleJudgeInfo {
    pub id: i32,
    pub game_id: i32,
    pub title: String,
}

pub async fn get_puzzle_judge_info(
    db_pool: &DbPool,
    puzzle_id: i32,
) -> Result<Option<PuzzleJudgeInfo>, RbInternalError> {
    let result = sqlx::query_as!(
        PuzzleJudgeInfo,
        "SELECT p.id, r.game_id, p.title
        FROM rb_puzzle p
        JOIN rb_round r ON r.id = p.round_id
        WHERE p.id = $1;",
        puzzle_id
    )
    .fetch_optional(db_pool)
    .await?;

    Ok(result)
}

pub async fn get_puzzle_id_by_game_ref(
    db_pool: &DbPool,
    game_id: i32,
    puzzle_ref: &str,
) -> Result<Option<i32>, RbInternalError> {
    let result = if let Ok(puzzle_id) = puzzle_ref.parse::<i32>() {
        sqlx::query_scalar!(
            "SELECT id FROM rb_puzzle
            WHERE game_id = $1 AND id = $2;",
            game_id,
            puzzle_id
        )
        .fetch_optional(db_pool)
        .await?
    } else {
        sqlx::query_scalar!(
            "SELECT id FROM rb_puzzle
            WHERE game_id = $1 AND slug = $2;",
            game_id,
            puzzle_ref
        )
        .fetch_optional(db_pool)
        .await?
    };

    Ok(result)
}

pub async fn get_hint_puzzle(
    db_pool: &DbPool,
    _kv_pool: &KvPool,
    hint_id: i32,
) -> Result<Option<i32>, RbInternalError> {
    let result = sqlx::query_scalar!("SELECT puzzle_id FROM rb_hint WHERE id = $1;", hint_id)
        .fetch_optional(db_pool)
        .await?;

    Ok(result)
}

pub async fn can_team_access_puzzle(
    db_pool: &DbPool,
    team_id: i32,
    puzzle_id: i32,
) -> Result<bool, RbInternalError> {
    let result = sqlx::query_scalar!(
        "SELECT EXISTS (
            SELECT 1
            FROM rb_team_puzzle tp
            JOIN rb_team t ON t.id = tp.team_id
            JOIN rb_puzzle p ON p.id = tp.puzzle_id
            JOIN rb_puzzle_effective_release rp ON rp.puzzle_id = p.id
            WHERE tp.team_id = $1
                AND tp.puzzle_id = $2
                AND NOT t.is_banned
                AND tp.state >= 0
                AND rp.release_at <= NOW()
        );",
        team_id,
        puzzle_id
    )
    .fetch_one(db_pool)
    .await?
    .unwrap_or(false);

    Ok(result)
}

pub async fn get_puzzle_user_info(
    db_pool: &DbPool,
    user_id: i32,
    puzzle_id: i32,
    user_role: RbUserRole,
) -> Result<Option<GameUserInfo>, RbInternalError> {
    let Some(game_id) = get_puzzle_game(db_pool, puzzle_id).await? else {
        return Ok(None);
    };

    let Some(team_id) = db::game::get_game_user_info(db_pool, user_id, game_id, user_role)
        .await?
        .and_then(|info| info.team_id)
    else {
        return Ok(None);
    };

    let access = can_team_access_puzzle(db_pool, team_id, puzzle_id).await?;

    match access {
        true => Ok(Some(GameUserInfo {
            game_id,
            team_id: Some(team_id),
        })),
        false => Ok(None),
    }
}

pub async fn get_hint_user_info(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    user_id: i32,
    hint_id: i32,
    user_role: RbUserRole,
) -> Result<Option<GameUserInfo>, RbInternalError> {
    let puzzle_id = get_hint_puzzle(db_pool, kv_pool, hint_id).await?;
    if puzzle_id.is_none() {
        return Ok(None);
    }

    get_puzzle_user_info(db_pool, user_id, puzzle_id.unwrap(), user_role).await
}

#[derive(FromRow, Serialize)]
pub struct RbPuzzleShowRoundData {
    pub id: i32,
    pub slug: Option<String>,
    pub title: String,
}

#[derive(FromRow, Serialize)]
pub struct RbPuzzleShowData {
    pub id: i32,
    pub slug: Option<String>,
    pub title: String,
    pub ptype: RbPuzzleType,
    pub content: String,
    pub content_type: RbContentType,
    pub round: RbPuzzleShowRoundData,
    pub game_id: i32,
    pub announcements: Vec<db::anmt::RbAnnouncementShowData>,
}

pub async fn get_puzzle_show(
    db_pool: &DbPool,
    puzzle_id: i32,
) -> Result<Option<RbPuzzleShowData>, RbInternalError> {
    let result = sqlx::query!(
        "SELECT p.id, p.slug, p.title, p.ptype, p.content, p.content_type,
                p.round_id, r.slug AS round_slug, r.title AS round_title, r.game_id
        FROM rb_puzzle p
        JOIN rb_round r ON r.id = p.round_id AND r.puzzle IS DISTINCT FROM p.id
        WHERE p.id = $1;",
        puzzle_id
    )
    .fetch_optional(db_pool)
    .await?;

    Ok(result.map(|x| RbPuzzleShowData {
        id: x.id,
        slug: x.slug,
        title: x.title,
        ptype: x.ptype.into(),
        content: x.content,
        content_type: x.content_type.into(),
        round: RbPuzzleShowRoundData {
            id: x.round_id,
            slug: x.round_slug,
            title: x.round_title,
        },
        game_id: x.game_id,
        announcements: Vec::new(),
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

#[derive(Clone, FromRow, Serialize)]
pub struct RbPuzzleTeamStateShowData {
    pub state: RbTeamPuzzleState,
    pub max_submit: Option<i32>,
    pub submit_count: i64,
    pub answers: Vec<String>,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub utime_at: OffsetDateTime,
    #[serde(with = "crate::serde_helpers::serialize_option_offset_datetime")]
    pub cooldown_till: Option<OffsetDateTime>,
}

pub async fn get_puzzle_team_state(
    db_pool: &DbPool,
    team_id: i32,
    puzzle_id: i32,
) -> Result<Option<RbPuzzleTeamStateShowData>, RbInternalError> {
    let row = sqlx::query!(
        "SELECT GREATEST(tp.ctime_at, rp.release_at) AS \"utime_at!\",
                tp.state, tp.cooldown_till,
                tp.max_submit + p.max_submit AS max_submit,
                COUNT(DISTINCT fs.id) AS submit_count,
                ARRAY_AGG(DISTINCT s.real_answer) FILTER (WHERE s.real_answer IS NOT NULL) AS answers
        FROM rb_team_puzzle tp
        JOIN rb_puzzle p ON p.id = tp.puzzle_id
        JOIN rb_puzzle_effective_release rp ON rp.puzzle_id = p.id
        LEFT JOIN rb_submission fs ON fs.puzzle_id = tp.puzzle_id
            AND fs.team_id = tp.team_id
            AND fs.saction = 0
            AND NOT fs.ignored
        LEFT JOIN rb_submission s ON s.puzzle_id = tp.puzzle_id
            AND s.team_id = tp.team_id
            AND s.saction = 1
        WHERE tp.team_id = $1 AND tp.puzzle_id = $2
            AND tp.state >= 0
            AND rp.release_at <= NOW()
        GROUP BY GREATEST(tp.ctime_at, rp.release_at),
            tp.state, tp.max_submit, tp.cooldown_till, p.max_submit;",
        team_id,
        puzzle_id
    )
    .fetch_optional(db_pool)
    .await?;

    if row.is_none() {
        return Ok(None);
    }
    let row = row.unwrap();

    Ok(Some(RbPuzzleTeamStateShowData {
        state: row.state.into(),
        max_submit: row.max_submit,
        submit_count: row.submit_count.unwrap_or(0),
        answers: row.answers.unwrap_or_default(),
        utime_at: row.utime_at,
        cooldown_till: row.cooldown_till,
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

async fn invalidate_team_puzzle_state_cache(
    app: &AppState,
    team_id: i32,
    puzzle_id: i32,
) -> Result<(), RbInternalError> {
    let round_ids = sqlx::query_scalar!("SELECT id FROM rb_round WHERE puzzle = $1;", puzzle_id)
        .fetch_all(&app.db)
        .await?;

    if round_ids.is_empty() {
        db::cache::invalidate_team_puzzle(app, team_id, puzzle_id).await?;
    } else {
        for round_id in round_ids {
            db::cache::invalidate_team_round(app, team_id, round_id).await?;
        }
    }

    Ok(())
}

pub async fn get_puzzle_show_str_for_team(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    team_id: i32,
    puzzle_id: i32,
) -> Result<Option<String>, RbInternalError> {
    if !can_team_access_puzzle(db_pool, team_id, puzzle_id).await? {
        return Ok(None);
    }

    if let Some(show_str) = get_puzzle_show_str(db_pool, kv_pool, puzzle_id).await? {
        let mut show: Value = serde_json::from_str(&show_str)?;
        let announcements = db::anmt::list_for_team_puzzle(db_pool, team_id, puzzle_id).await?;
        if let Some(show) = show.as_object_mut() {
            show.insert(
                "announcements".to_owned(),
                serde_json::to_value(announcements)?,
            );
        }
        let show_str = serde_json::to_string(&show)?;
        let json = match get_puzzle_team_state_str(db_pool, kv_pool, team_id, puzzle_id).await? {
            Some(state_str) => format!("{{\"data\":{show_str},\"state\":{state_str}}}"),
            None => format!("{{\"data\":{show_str}}}"),
        };
        Ok(Some(json))
    } else {
        Ok(None)
    }
}

#[derive(Clone, Serialize)]
pub struct SubmitStateUpdate {
    pub state: Option<RbPuzzleTeamStateShowData>,
    pub currency: Vec<db::team::RbCurrencyShowData>,
    pub currency_penalty: Vec<CurrencyPenaltyShowData>,
}

#[derive(Clone)]
pub struct SubmitStateBox(pub Box<SubmitStateUpdate>);

#[derive(Clone, Serialize)]
pub struct BackendSubmissionInput {
    pub user_answer: String,
    pub norm_answer: Option<String>,
    pub saction: RbJudgeAction,
    pub sresult: Option<String>,
    pub real_answer: Option<String>,
    pub ignored: bool,
}

#[derive(FromRow, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct BackendSubmissionShowData {
    pub id: i32,
    pub team_id: i32,
    pub user_id: i32,
    pub puzzle_id: i32,
    pub user_answer: String,
    pub norm_answer: String,
    pub saction: RbJudgeAction,
    pub sresult: Option<String>,
    pub real_answer: Option<String>,
    pub ignored: bool,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub ctime_at: OffsetDateTime,
}

pub async fn add_backend_submission(
    pool: &DbPool,
    team_id: i32,
    user_id: i32,
    puzzle_id: i32,
    data: &BackendSubmissionInput,
) -> Result<BackendSubmissionShowData, RbInternalError> {
    let norm_answer = data
        .norm_answer
        .clone()
        .unwrap_or_else(|| normalize_answer(&data.user_answer));

    let row = sqlx::query_as!(
        BackendSubmissionShowData,
        r#"INSERT INTO rb_submission
            (team_id, user_id, puzzle_id, user_answer, norm_answer, saction, sresult, real_answer, ignored)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        RETURNING id, team_id, user_id, puzzle_id, user_answer, norm_answer,
            saction, sresult, real_answer, ignored, ctime_at"#,
        team_id,
        user_id,
        puzzle_id,
        data.user_answer,
        norm_answer,
        i16::from(data.saction),
        data.sresult,
        data.real_answer,
        data.ignored
    )
    .fetch_one(pool)
    .await?;

    Ok(row)
}

pub async fn add_backend_submission_and_invalidate(
    app: &AppState,
    team_id: i32,
    user_id: i32,
    puzzle_id: i32,
    data: &BackendSubmissionInput,
) -> Result<BackendSubmissionShowData, RbInternalError> {
    let row = add_backend_submission(&app.db, team_id, user_id, puzzle_id, data).await?;
    if let Some(puzzle_info) = get_puzzle_judge_info(&app.db, puzzle_id).await? {
        db::event_log::insert_pool(
            &app.db,
            db::event_log::EventLogInput {
                event_type: "submission.backend_added",
                event_scope: i16::from(db::event_log::EventScope::TeamActivity),
                severity: i16::from(db::event_log::EventSeverity::Info),
                game_id: Some(puzzle_info.game_id),
                team_id: Some(team_id),
                user_id: Some(user_id),
                puzzle_id: Some(puzzle_id),
                submission_id: Some(row.id),
                data: json!({
                    "submission": {
                        "id": row.id,
                        "answer": row.user_answer,
                        "norm_answer": row.norm_answer,
                        "action": i16::from(row.saction),
                        "result": row.sresult,
                        "ignored": row.ignored
                    },
                    "puzzle": {
                        "id": puzzle_info.id,
                        "title": puzzle_info.title
                    },
                    "source": "backend"
                }),
                ..Default::default()
            },
        )
        .await?;
    }
    invalidate_team_puzzle_state_cache(app, team_id, puzzle_id).await?;
    Ok(row)
}

pub async fn solve_backend_puzzle(
    app: &AppState,
    team_id: i32,
    user_id: i32,
    puzzle_id: i32,
    submission_id: i32,
) -> Result<bool, RbInternalError> {
    let mut tx = app.db.begin().await?;

    let submission = sqlx::query_as!(
        BackendSubmissionShowData,
        r#"SELECT id, team_id, user_id, puzzle_id, user_answer, norm_answer,
            saction, sresult, real_answer, ignored, ctime_at
        FROM rb_submission
        WHERE id = $1 AND team_id = $2 AND puzzle_id = $3
        FOR UPDATE;"#,
        submission_id,
        team_id,
        puzzle_id
    )
    .fetch_one(&mut *tx)
    .await?;

    let solved = sqlx::query!(
        "UPDATE rb_team_puzzle SET state = 1, solve_at = NOW()
        WHERE team_id = $1 AND puzzle_id = $2 AND state = 0",
        team_id,
        puzzle_id
    )
    .execute(&mut *tx)
    .await?
    .rows_affected()
        > 0;

    if matches!(submission.saction, RbJudgeAction::FinishGame) {
        sqlx::query!(
            "UPDATE rb_team SET finish_at = COALESCE(finish_at, NOW())
            WHERE id = $1 AND is_locked;",
            team_id
        )
        .execute(&mut *tx)
        .await?;
    }

    if solved {
        db::ticket::close_puzzle_tickets_on_solve(&mut tx, team_id, puzzle_id, user_id).await?;
    }

    tx.commit().await?;

    db::cache::invalidate_team_puzzle(app, team_id, puzzle_id).await?;
    if solved {
        db::cache::invalidate_team_puzzle_solved(app, team_id, puzzle_id).await?;
        if let Some(puzzle_info) = get_puzzle_judge_info(&app.db, puzzle_id).await? {
            db::event_log::insert_pool(
                &app.db,
                db::event_log::EventLogInput {
                    event_type: "puzzle.backend_solved",
                    event_scope: i16::from(db::event_log::EventScope::TeamActivity),
                    severity: i16::from(db::event_log::EventSeverity::Info),
                    game_id: Some(puzzle_info.game_id),
                    team_id: Some(team_id),
                    user_id: Some(user_id),
                    puzzle_id: Some(puzzle_id),
                    submission_id: Some(submission_id),
                    data: json!({
                        "puzzle": {
                            "id": puzzle_info.id,
                            "title": puzzle_info.title
                        },
                        "submission": {
                            "id": submission_id
                        },
                        "source": "backend"
                    }),
                    ..Default::default()
                },
            )
            .await?;
        }
        if matches!(submission.saction, RbJudgeAction::FinishGame) {
            db::cache::invalidate_team_info(app, team_id).await?;
        }
        let _ = unlock_new_puzzles(app, team_id).await?;
    }

    Ok(solved)
}

pub async fn solve_backend_puzzle_with_submission(
    app: &AppState,
    team_id: i32,
    user_id: i32,
    puzzle_id: i32,
    submission: &BackendSubmissionShowData,
) -> Result<bool, RbInternalError> {
    if submission.team_id != team_id
        || submission.user_id != user_id
        || submission.puzzle_id != puzzle_id
    {
        return Err(RbInternalError::Other(
            "submission does not match current runtime".to_string(),
        ));
    }
    solve_backend_puzzle(app, team_id, user_id, puzzle_id, submission.id).await
}

#[derive(Clone, Serialize)]
pub struct CurrencyPenaltyShowData {
    pub currency_id: i32,
    pub name: String,
    pub prec: i32,
    pub amount: i64,
}

pub async fn get_judge_rules(
    pool: &DbPool,
    puzzle_id: i32,
) -> Result<Option<Arc<Vec<JudgeRule>>>, RbInternalError> {
    if let Some(c) = JUDGE_CACHE.get(&puzzle_id) {
        return Ok(Some(c.clone()));
    }

    let judge = sqlx::query_scalar!("SELECT judge FROM rb_puzzle WHERE id = $1;", puzzle_id)
        .fetch_optional(pool)
        .await?;

    if judge.is_none() {
        return Ok(None);
    }

    let rules = game::judge::value_to_judge(judge.unwrap())?;

    let rules = Arc::new(rules);
    JUDGE_CACHE.insert(puzzle_id, rules.clone());

    Ok(Some(rules))
}

type UnlockCondMap = DashMap<i32, Arc<Vec<(i32, GateExpr)>>>;
static UNLOCK_COND_CACHE: Lazy<UnlockCondMap> = Lazy::new(DashMap::new);

pub fn invalidate_admin_cache(game_id: i32, puzzle_id: i32) {
    JUDGE_CACHE.remove(&puzzle_id);
    UNLOCK_COND_CACHE.remove(&game_id);
}

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
        WHERE g.id = $1
        ORDER BY r.sort, r.id, (p.id IS DISTINCT FROM r.puzzle), p.sort, p.id;",
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
    Ok {
        result: JudgeResult,
        solved: bool,
        unlocks: Vec<i32>,
        cooldown_till: Option<OffsetDateTime>,
        update: SubmitStateBox,
    },
    Locked,
    Duplicate,
    Invalid,
    NotFound,
}

#[derive(Deserialize)]
struct PenaltyRule {
    #[serde(rename = "type")]
    rtype: RbPuzzlePenaltyType,
    args: Vec<i64>,
}

pub async fn submit_answer(
    app: &AppState,
    user: &AuthUser,
    puzzle_id: i32,
    answer: &str,
) -> Result<SubmitAnswerResult, RbInternalError> {
    let norm_answer = normalize_answer(answer);
    if norm_answer.is_empty() {
        return Ok(SubmitAnswerResult::Invalid);
    }

    let mut tx = app.db.begin().await?;

    let team_id = user.req_team_id()?.ok_or("Require team_id")?;

    let access = sqlx::query_scalar!(
        "SELECT tp.state >= 0 AND rp.release_at <= NOW() AND NOT t.is_banned AS \"access!\"
        FROM rb_team_puzzle tp
        JOIN rb_team t ON t.id = tp.team_id
        JOIN rb_puzzle p ON p.id = tp.puzzle_id
        JOIN rb_puzzle_effective_release rp ON rp.puzzle_id = p.id
        WHERE tp.team_id = $1 AND tp.puzzle_id = $2
        FOR UPDATE OF tp;",
        team_id,
        puzzle_id
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(access) = access else {
        return Ok(SubmitAnswerResult::NotFound);
    };
    if !access {
        return Ok(SubmitAnswerResult::Locked);
    }

    let allowed = sqlx::query_scalar!(
        "SELECT (tp.cooldown_till IS NULL OR tp.cooldown_till <= NOW())
            AND (p.max_submit IS NULL OR COUNT(s.id) < p.max_submit + tp.max_submit)
        FROM rb_team_puzzle tp
        JOIN rb_puzzle p ON p.id = tp.puzzle_id
        LEFT JOIN rb_submission s ON s.team_id = tp.team_id
            AND s.puzzle_id = tp.puzzle_id
            AND s.saction = 0
            AND NOT s.ignored
        WHERE tp.team_id = $1 AND tp.puzzle_id = $2
        GROUP BY tp.cooldown_till, p.max_submit, tp.max_submit;",
        team_id,
        puzzle_id
    )
    .fetch_one(&mut *tx)
    .await?;

    if !allowed.unwrap_or(false) {
        return Ok(SubmitAnswerResult::Locked);
    }

    let submit_row = sqlx::query_as!(
        BackendSubmissionShowData,
        r#"INSERT INTO rb_submission (team_id, user_id, puzzle_id, user_answer, norm_answer)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT DO NOTHING
        RETURNING id, team_id, user_id, puzzle_id, user_answer, norm_answer,
            saction, sresult, real_answer, ignored, ctime_at"#,
        team_id,
        user.uid,
        puzzle_id,
        answer,
        norm_answer
    )
    .fetch_optional(&mut *tx)
    .await?;

    if submit_row.is_none() {
        return Ok(SubmitAnswerResult::Duplicate);
    }
    let submit_row = submit_row.unwrap();
    let submit_id = submit_row.id;
    let submit_ctime_at = submit_row.ctime_at;

    let puzzle_info = sqlx::query!(
        "SELECT id, game_id, round_id, title
        FROM rb_puzzle
        WHERE id = $1;",
        puzzle_id
    )
    .fetch_one(&mut *tx)
    .await?;
    let judge = get_judge_rules(&app.db, puzzle_id).await?;
    if judge.is_none() {
        return Ok(SubmitAnswerResult::NotFound);
    }

    let rules = judge.unwrap();
    let norm_answer_for_rules = norm_answer.clone();
    let norm_answer_for_custom = norm_answer.clone();

    let result = judge_by_rules(&rules, &norm_answer_for_rules, move |rule| {
        let app = app.clone();
        let submit_row = submit_row.clone();
        let backend_name = rule.function.clone();
        let norm_answer = norm_answer_for_custom.clone();
        async move {
            let backend_name = backend_name.ok_or_else(|| {
                RbInternalError::Other("custom judge function is missing".to_string())
            })?;
            let backend = db::puzzle_backend::get_backend(&app.db, puzzle_id)
                .await?
                .ok_or(RbInternalError::Other("backend not found".to_string()))?;
            let puzzle_info = get_puzzle_judge_info(&app.db, puzzle_id)
                .await?
                .ok_or(RbInternalError::Other("puzzle not found".to_string()))?;
            let user_info = db::user::get_display_by_id(&app.db, user.uid).await?;
            let team_info = db::team::get_by_id_show(&app.db, team_id).await?;
            let submit_row = submit_row.clone();
            let team_info =
                team_info.ok_or(RbInternalError::Other("team not found".to_string()))?;

            let output = crate::module::puzzle_backend_js::execute_judge(
                &app,
                backend,
                backend_name,
                crate::module::puzzle_backend_js::JudgeRuntimeContext {
                    puzzle_id: puzzle_info.id,
                    game_id: puzzle_info.game_id,
                    puzzle_title: puzzle_info.title,
                    team_id,
                    team_name: team_info.name,
                    user_id: user.uid,
                    user_nickname: user_info.nickname,
                    user_answer: answer.to_string(),
                    norm_answer,
                    submission: submit_row,
                },
            )
            .await?;

            Ok(output)
        }
    })
    .await?;

    sqlx::query!(
        "UPDATE rb_submission
        SET saction = $1, sresult = $2, real_answer = $3, ignored = $5
        WHERE id = $4;",
        i16::from(result.action),
        result.result,
        result.answer,
        submit_id,
        result.ignored || matches!(result.action, RbJudgeAction::Error)
    )
    .execute(&mut *tx)
    .await?;

    db::event_log::insert_tx(
        &mut tx,
        db::event_log::EventLogInput {
            event_type: "submission.judged",
            event_scope: i16::from(db::event_log::EventScope::TeamActivity),
            severity: if matches!(result.action, RbJudgeAction::Error) {
                i16::from(db::event_log::EventSeverity::Warning)
            } else {
                i16::from(db::event_log::EventSeverity::Info)
            },
            game_id: Some(puzzle_info.game_id),
            team_id: Some(team_id),
            user_id: Some(user.uid),
            puzzle_id: Some(puzzle_id),
            round_id: Some(puzzle_info.round_id),
            submission_id: Some(submit_id),
            data: json!({
                "submission": {
                    "id": submit_id,
                    "answer": answer,
                    "norm_answer": norm_answer,
                    "action": i16::from(result.action),
                    "result": result.result,
                    "ignored": result.ignored || matches!(result.action, RbJudgeAction::Error)
                },
                "puzzle": {
                    "id": puzzle_info.id,
                    "title": puzzle_info.title
                }
            }),
            ..Default::default()
        },
    )
    .await?;

    let mut solved = false;
    let mut cooldown_till: Option<OffsetDateTime> = None;
    let mut cooldown_seconds: Option<i64> = None;
    let mut do_unlock = false;
    let mut currency_updated = false;
    let mut currency_penalty: Vec<CurrencyPenaltyShowData> = vec![];
    let mut state_cache_invalidated = false;

    match result.action {
        RbJudgeAction::Correct | RbJudgeAction::FinishGame => {
            let update = sqlx::query!(
                "UPDATE rb_team_puzzle SET state = 1, solve_at = NOW()
                WHERE team_id = $1 AND puzzle_id = $2 AND state = 0",
                team_id,
                puzzle_id
            )
            .execute(&mut *tx)
            .await?;

            if matches!(result.action, RbJudgeAction::FinishGame) {
                sqlx::query!(
                    "UPDATE rb_team SET finish_at = COALESCE(finish_at, NOW())
                    WHERE id = $1 AND is_locked;",
                    team_id
                )
                .execute(&mut *tx)
                .await?;
            }

            if update.rows_affected() > 0 {
                solved = true;
                do_unlock = true;
                db::event_log::insert_tx(
                    &mut tx,
                    db::event_log::EventLogInput {
                        event_type: "puzzle.solved",
                        event_scope: i16::from(db::event_log::EventScope::TeamActivity),
                        severity: i16::from(db::event_log::EventSeverity::Info),
                        game_id: Some(puzzle_info.game_id),
                        team_id: Some(team_id),
                        user_id: Some(user.uid),
                        puzzle_id: Some(puzzle_id),
                        round_id: Some(puzzle_info.round_id),
                        submission_id: Some(submit_id),
                        data: json!({
                            "puzzle": {
                                "id": puzzle_info.id,
                                "title": puzzle_info.title
                            },
                            "submission": {
                                "id": submit_id,
                                "answer": answer
                            }
                        }),
                        ..Default::default()
                    },
                )
                .await?;
                db::ticket::close_puzzle_tickets_on_solve(&mut tx, team_id, puzzle_id, user.uid)
                    .await?;
            }
        }
        RbJudgeAction::StartGame => {
            let currency_feature = sqlx::query!(
                "SELECT state, utime_at FROM rb_game_feature
                WHERE game_id = $1 AND feature_type = 4
                FOR UPDATE;",
                puzzle_info.game_id
            )
            .fetch_one(&mut *tx)
            .await?;
            let currency_start_at = if currency_feature.state == 1 {
                submit_ctime_at.max(currency_feature.utime_at)
            } else {
                submit_ctime_at
            };

            let result = sqlx::query!(
                "UPDATE rb_team SET is_locked = TRUE
                WHERE id = $1 AND NOT is_locked;",
                team_id
            )
            .execute(&mut *tx)
            .await?;

            if result.rows_affected() > 0 {
                db::event_log::insert_tx(
                    &mut tx,
                    db::event_log::EventLogInput {
                        event_type: "game.started",
                        event_scope: i16::from(db::event_log::EventScope::TeamActivity),
                        severity: i16::from(db::event_log::EventSeverity::Info),
                        game_id: Some(puzzle_info.game_id),
                        team_id: Some(team_id),
                        user_id: Some(user.uid),
                        puzzle_id: Some(puzzle_id),
                        round_id: Some(puzzle_info.round_id),
                        submission_id: Some(submit_id),
                        data: json!({
                            "puzzle": {
                                "id": puzzle_info.id,
                                "title": puzzle_info.title
                            },
                            "submission": {
                                "id": submit_id,
                                "answer": answer
                            }
                        }),
                        ..Default::default()
                    },
                )
                .await?;

                do_unlock = true;
            }

            sqlx::query!(
                "INSERT INTO rb_team_currency (team_id, currency_id, amount, hidden, utime_at)
                SELECT t.id AS team_id, c.id AS currency_id, c.init_amount AS amount,
                    c.init_hidden AS hidden, $2
                FROM rb_team t
                JOIN rb_currency c ON c.game_id = t.game_id
                WHERE t.id = $1
                ON CONFLICT (team_id, currency_id) DO NOTHING;",
                team_id,
                currency_start_at
            )
            .execute(&mut *tx)
            .await?;
        }
        RbJudgeAction::Fail => {
            let info = sqlx::query!(
                "SELECT
                    (SELECT COUNT(*) FROM rb_submission
                        WHERE team_id = $1 AND puzzle_id = $2
                            AND saction = 0
                            AND NOT ignored)
                        AS failure_count,
                    p.penalty,
                    p.id AS puzzle_id
                FROM rb_puzzle p
                WHERE p.id = $2
                GROUP BY p.id, p.penalty;",
                team_id,
                puzzle_id
            )
            .fetch_one(&mut *tx)
            .await?;

            let failure_count = info.failure_count.unwrap_or(0);
            let rules: Vec<PenaltyRule> = serde_json::from_value(info.penalty)?;
            for rule in rules {
                match rule.rtype {
                    RbPuzzlePenaltyType::FixedTime => {
                        if let Some(x) = rule.args.first() {
                            cooldown_seconds = Some(*x);
                        }
                    }
                    RbPuzzlePenaltyType::LinearTime => {
                        if let Some(x) = rule.args.first() {
                            cooldown_seconds = Some((*x).saturating_mul(failure_count));
                        }
                    }
                    RbPuzzlePenaltyType::Currency => {
                        if let Some(currency_id) = rule.args.first()
                            && let Some(amount) = rule.args.get(1)
                            && let Ok(currency_id) = i32::try_from(*currency_id)
                            && let Some(penalty_row) = sqlx::query!(
                                r#"WITH current AS (
                                    SELECT tc.team_id, c.id, c.slug, c.cname, c.prec,
                                        LEAST(
                                            tc.amount::NUMERIC + FLOOR(EXTRACT(EPOCH FROM (NOW() - tc.utime_at)) / 60) * (c.growth + tc.growth)::NUMERIC,
                                            c.max_amount::NUMERIC
                                        )::BIGINT AS current_amount
                                    FROM rb_team_currency tc
                                    JOIN rb_currency c ON tc.currency_id = c.id
                                    WHERE tc.team_id = $2 AND c.id = $3
                                    FOR UPDATE
                                ), updated AS (
                                    UPDATE rb_team_currency tc
                                    SET utime_at = NOW(), amount = current.current_amount - $1
                                    FROM current
                                    WHERE tc.team_id = current.team_id AND tc.currency_id = current.id
                                    RETURNING current.id, current.slug, current.cname, current.prec,
                                        current.current_amount, tc.amount
                                )
                                SELECT id AS "currency_id!", slug AS "slug!", cname AS "name!",
                                    prec AS "prec!", $1::BIGINT AS "amount!",
                                    current_amount AS "before!", amount AS "after!"
                                FROM updated;"#,
                                amount,
                                team_id,
                                currency_id
                            )
                            .fetch_optional(&mut *tx)
                            .await?
                            {
                                let penalty = CurrencyPenaltyShowData {
                                    currency_id: penalty_row.currency_id,
                                    name: penalty_row.name.clone(),
                                    prec: penalty_row.prec,
                                    amount: penalty_row.amount,
                                };
                                currency_updated = true;
                                db::event_log::insert_tx(
                                    &mut tx,
                                    db::event_log::EventLogInput {
                                        event_type: "currency.penalty",
                                        event_scope: i16::from(db::event_log::EventScope::TeamActivity),
                                        severity: i16::from(db::event_log::EventSeverity::Info),
                                        game_id: Some(puzzle_info.game_id),
                                        team_id: Some(team_id),
                                        user_id: Some(user.uid),
                                        puzzle_id: Some(puzzle_id),
                                        round_id: Some(puzzle_info.round_id),
                                        submission_id: Some(submit_id),
                                        currency_id: Some(penalty.currency_id),
                                        delta_amount: Some(-penalty.amount),
                                        data: json!({
                                            "reason": "puzzle.penalty",
                                            "currency": {
                                                "id": penalty.currency_id,
                                                "slug": penalty_row.slug,
                                                "name": penalty.name,
                                                "prec": penalty.prec
                                            },
                                            "delta": -penalty.amount,
                                            "before": penalty_row.before,
                                            "after": penalty_row.after,
                                            "submission": {
                                                "id": submit_id
                                            },
                                            "puzzle": {
                                                "id": puzzle_info.id,
                                                "title": puzzle_info.title
                                            }
                                        }),
                                        ..Default::default()
                                    },
                                )
                                .await?;
                                currency_penalty.push(penalty);
                            }
                    }
                    _ => {}
                }
            }
            if let Some(time) = cooldown_seconds {
                cooldown_till = sqlx::query_scalar!(
                    "UPDATE rb_team_puzzle
                    SET cooldown_till = NOW() + ($1::BIGINT * INTERVAL '1 second')
                    WHERE team_id = $2 AND puzzle_id = $3
                    RETURNING cooldown_till;",
                    time,
                    team_id,
                    puzzle_id
                )
                .fetch_one(&mut *tx)
                .await?;
            }

            invalidate_team_puzzle_state_cache(app, team_id, info.puzzle_id).await?;
            state_cache_invalidated = true;
        }
        _ => {}
    }

    if matches!(result.action, RbJudgeAction::Fail) {
        let consequences = json!({
            "cooldown_seconds": cooldown_seconds,
            "currency_penalty": &currency_penalty
        });
        sqlx::query!(
            "UPDATE rb_event_log
            SET data = data || $1::JSONB
            WHERE submission_id = $2 AND event_type = 'submission.judged';",
            consequences,
            submit_id
        )
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    if matches!(result.action, RbJudgeAction::Fail) && !state_cache_invalidated {
        invalidate_team_puzzle_state_cache(app, team_id, puzzle_id).await?;
    }

    if result.action.side_effect() {
        db::cache::invalidate_team_puzzle_solved(app, team_id, puzzle_id).await?;
    }

    let unlocks = if do_unlock {
        unlock_new_puzzles(app, team_id).await?
    } else {
        vec![]
    };
    let update = SubmitStateBox(Box::new(SubmitStateUpdate {
        state: get_puzzle_team_state(&app.db, team_id, puzzle_id).await?,
        currency: if currency_updated || matches!(result.action, RbJudgeAction::StartGame) {
            db::team::get_currency_info(&app.db, team_id).await?
        } else {
            vec![]
        },
        currency_penalty,
    }));

    Ok(SubmitAnswerResult::Ok {
        result,
        solved,
        unlocks,
        cooldown_till,
        update,
    })
}

struct RbPuzzleStates {
    solved: HashSet<i32>,
    puzzle_slugs: HashMap<String, u32>,
    round_slugs: HashMap<String, u32>,
    round_puzzles: HashMap<u32, Vec<u32>>,
    game_started: bool,
}

impl PuzzleStates for RbPuzzleStates {
    fn is_solved(&self, id: expr::types::PuzzleId) -> bool {
        self.solved.contains(&id.try_into().unwrap_or(i32::MAX))
    }

    fn solved(&self) -> Vec<expr::types::PuzzleId> {
        self.solved
            .iter()
            .map(|&x| x.try_into().unwrap_or(0))
            .collect()
    }

    fn puzzle_slug(&self, slug: &str) -> Option<expr::types::PuzzleId> {
        self.puzzle_slugs.get(slug).copied()
    }

    fn round_slug(&self, slug: &str) -> Option<expr::types::RoundId> {
        self.round_slugs.get(slug).copied()
    }

    fn round_puzzles(&self, id: expr::types::RoundId) -> Option<Vec<expr::types::PuzzleId>> {
        self.round_puzzles.get(&id).cloned()
    }

    fn game_started(&self) -> bool {
        self.game_started
    }
}

pub async fn unlock_new_puzzles(app: &AppState, team_id: i32) -> Result<Vec<i32>, RbInternalError> {
    let info = sqlx::query!(
        "SELECT t.game_id, t.is_locked, tp.puzzle_id AS \"puzzle_id?\"
        FROM rb_team t
        LEFT JOIN rb_team_puzzle tp ON tp.team_id = t.id AND tp.state >= 1
        WHERE t.id = $1;",
        team_id
    )
    .fetch_all(&app.db)
    .await?;

    // dont know if possible but we just protect from it
    if info.is_empty() {
        return Ok(vec![]);
    }

    let game_id = info[0].game_id;
    let solved = info.iter().filter_map(|r| r.puzzle_id).collect();

    let round_rows = sqlx::query!(
        "SELECT id, slug
        FROM rb_round
        WHERE game_id = $1;",
        game_id
    )
    .fetch_all(&app.db)
    .await?;

    let puzzle_rows = sqlx::query!(
        "SELECT p.id, p.slug, p.round_id, r.puzzle AS round_puzzle_id
        FROM rb_puzzle p
        JOIN rb_round r ON r.id = p.round_id
        WHERE r.game_id = $1
        ORDER BY r.sort, r.id, (p.id IS DISTINCT FROM r.puzzle), p.sort, p.id;",
        game_id
    )
    .fetch_all(&app.db)
    .await?;

    let mut round_slugs: HashMap<String, u32> = HashMap::new();
    for row in round_rows {
        if let Some(slug) = row.slug {
            round_slugs.insert(slug, row.id.try_into().unwrap_or(0));
        }
    }

    let mut puzzle_slugs: HashMap<String, u32> = HashMap::new();
    let mut round_puzzles: HashMap<u32, Vec<u32>> = HashMap::new();
    for row in puzzle_rows {
        let row_id = row.id;
        let puzzle_id = row_id.try_into().unwrap_or(0);
        if let Some(slug) = row.slug {
            puzzle_slugs.insert(slug, puzzle_id);
        }
        if row.round_puzzle_id != Some(row_id) {
            round_puzzles
                .entry(row.round_id.try_into().unwrap_or(0))
                .or_default()
                .push(puzzle_id);
        }
    }

    let state = RbPuzzleStates {
        solved,
        puzzle_slugs,
        round_slugs,
        round_puzzles,
        game_started: info[0].is_locked,
    };

    let conds = get_unlock_conds_by_game(&app.db, game_id).await?;

    let mut unlocks: Vec<i32> = Vec::new();

    for cond in conds.iter() {
        if !state.is_solved(cond.0.try_into().unwrap_or(0))
            && expr::ast::eval_compiled(&state, &cond.1)
        {
            unlocks.push(cond.0);
        }
    }

    if !unlocks.is_empty() {
        let inserted = sqlx::query!(
            "WITH inserted AS (
                INSERT INTO rb_team_puzzle (team_id, puzzle_id, state)
                SELECT $1, UNNEST($2::int[]), 0
                ON CONFLICT DO NOTHING
                RETURNING puzzle_id
            )
            SELECT p.id, p.title, p.round_id
            FROM inserted i
            JOIN rb_puzzle p ON p.id = i.puzzle_id;",
            team_id,
            &unlocks
        )
        .fetch_all(&app.db)
        .await?;

        for puzzle in inserted {
            db::event_log::insert_pool(
                &app.db,
                db::event_log::EventLogInput {
                    event_type: "puzzle.unlocked",
                    event_scope: i16::from(db::event_log::EventScope::TeamActivity),
                    severity: i16::from(db::event_log::EventSeverity::Info),
                    game_id: Some(game_id),
                    team_id: Some(team_id),
                    puzzle_id: Some(puzzle.id),
                    round_id: Some(puzzle.round_id),
                    data: json!({
                        "puzzle": {
                            "id": puzzle.id,
                            "title": puzzle.title
                        }
                    }),
                    ..Default::default()
                },
            )
            .await?;
        }
    }

    Ok(unlocks)
}

pub async fn admin_unlock_puzzle_for_eligible_teams(
    app: &AppState,
    puzzle_id: i32,
    game_id: i32,
    unlock_cond: &str,
) -> Result<Vec<i32>, RbInternalError> {
    let round_rows = sqlx::query!(
        "SELECT id, slug
        FROM rb_round
        WHERE game_id = $1;",
        game_id
    )
    .fetch_all(&app.db)
    .await?;

    let puzzle_rows = sqlx::query!(
        "SELECT p.id, p.slug, p.round_id, r.puzzle AS round_puzzle_id
        FROM rb_puzzle p
        JOIN rb_round r ON r.id = p.round_id
        WHERE r.game_id = $1
        ORDER BY r.sort, r.id, (p.id IS DISTINCT FROM r.puzzle), p.sort, p.id;",
        game_id
    )
    .fetch_all(&app.db)
    .await?;

    let mut round_slugs: HashMap<String, u32> = HashMap::new();
    for row in round_rows {
        if let Some(slug) = row.slug {
            round_slugs.insert(slug, row.id.try_into().unwrap_or(0));
        }
    }

    let mut puzzle_slugs: HashMap<String, u32> = HashMap::new();
    let mut round_puzzles: HashMap<u32, Vec<u32>> = HashMap::new();
    for row in puzzle_rows {
        let row_id = row.id;
        let current_puzzle_id = row_id.try_into().unwrap_or(0);
        if let Some(slug) = row.slug {
            puzzle_slugs.insert(slug, current_puzzle_id);
        }
        if row.round_puzzle_id != Some(row_id) {
            round_puzzles
                .entry(row.round_id.try_into().unwrap_or(0))
                .or_default()
                .push(current_puzzle_id);
        }
    }

    let compiled_unlock_cond = if unlock_cond == "default" {
        None
    } else {
        Some(expr::compile_gate_expr(unlock_cond).map_err(RbInternalError::Other)?)
    };

    let candidate_rows = sqlx::query!(
        "SELECT t.id, t.is_locked, solved.puzzle_id AS \"solved_puzzle_id?\"
        FROM rb_team t
        LEFT JOIN rb_team_puzzle current
            ON current.team_id = t.id AND current.puzzle_id = $1
        LEFT JOIN rb_team_puzzle solved
            ON solved.team_id = t.id AND solved.state >= 1
        WHERE t.game_id = $2 AND current.team_id IS NULL
        ORDER BY t.id;",
        puzzle_id,
        game_id
    )
    .fetch_all(&app.db)
    .await?;

    let mut eligible_team_ids = Vec::new();
    let mut current_team_id: Option<i32> = None;
    let mut current_team_locked = false;
    let mut solved = HashSet::new();

    let mut flush_team = |team_id: Option<i32>, team_locked: bool, solved: &HashSet<i32>| {
        let Some(team_id) = team_id else {
            return;
        };
        let state = RbPuzzleStates {
            solved: solved.clone(),
            puzzle_slugs: puzzle_slugs.clone(),
            round_slugs: round_slugs.clone(),
            round_puzzles: round_puzzles.clone(),
            game_started: team_locked,
        };
        let eligible = compiled_unlock_cond
            .as_ref()
            .is_none_or(|cond| expr::ast::eval_compiled(&state, cond));
        if eligible {
            eligible_team_ids.push(team_id);
        }
    };

    for row in candidate_rows {
        if current_team_id != Some(row.id) {
            flush_team(current_team_id, current_team_locked, &solved);
            current_team_id = Some(row.id);
            current_team_locked = row.is_locked;
            solved.clear();
        }

        if let Some(solved_puzzle_id) = row.solved_puzzle_id {
            solved.insert(solved_puzzle_id);
        }
    }
    flush_team(current_team_id, current_team_locked, &solved);

    if eligible_team_ids.is_empty() {
        return Ok(Vec::new());
    }

    let inserted_team_ids = sqlx::query_scalar!(
        "INSERT INTO rb_team_puzzle (team_id, puzzle_id, state)
        SELECT x.team_id, $2, 0
        FROM UNNEST($1::int[]) AS x(team_id)
        ON CONFLICT DO NOTHING
        RETURNING team_id;",
        &eligible_team_ids,
        puzzle_id
    )
    .fetch_all(&app.db)
    .await?;

    Ok(inserted_team_ids)
}

#[derive(Serialize)]
pub struct AdminClearPuzzleTeamStatesResult {
    pub team_count: usize,
    pub puzzle_states: usize,
    pub submissions: usize,
    pub hints: usize,
    pub tickets: usize,
    pub team_ids: Vec<i32>,
}

pub async fn admin_clear_puzzle_team_states(
    pool: &DbPool,
    puzzle_id: i32,
) -> Result<AdminClearPuzzleTeamStatesResult, RbInternalError> {
    let mut tx = pool.begin().await?;

    let puzzle_state_team_ids = sqlx::query_scalar!(
        "DELETE FROM rb_team_puzzle
        WHERE puzzle_id = $1
        RETURNING team_id;",
        puzzle_id
    )
    .fetch_all(&mut *tx)
    .await?;

    let submission_team_ids = sqlx::query_scalar!(
        "DELETE FROM rb_submission
        WHERE puzzle_id = $1
        RETURNING team_id;",
        puzzle_id
    )
    .fetch_all(&mut *tx)
    .await?;

    let hint_team_ids = sqlx::query_scalar!(
        "DELETE FROM rb_team_hint th
        USING rb_hint h
        WHERE th.hint_id = h.id AND h.puzzle_id = $1
        RETURNING th.team_id;",
        puzzle_id
    )
    .fetch_all(&mut *tx)
    .await?;

    let ticket_team_ids = sqlx::query_scalar!(
        "DELETE FROM rb_ticket
        WHERE puzzle_id = $1
        RETURNING team_id;",
        puzzle_id
    )
    .fetch_all(&mut *tx)
    .await?;

    tx.commit().await?;

    let mut team_ids = HashSet::new();
    team_ids.extend(puzzle_state_team_ids.iter().copied());
    team_ids.extend(submission_team_ids.iter().copied());
    team_ids.extend(hint_team_ids.iter().copied());
    team_ids.extend(ticket_team_ids.iter().copied());

    let mut team_ids = team_ids.into_iter().collect::<Vec<_>>();
    team_ids.sort_unstable();

    Ok(AdminClearPuzzleTeamStatesResult {
        team_count: team_ids.len(),
        puzzle_states: puzzle_state_team_ids.len(),
        submissions: submission_team_ids.len(),
        hints: hint_team_ids.len(),
        tickets: ticket_team_ids.len(),
        team_ids,
    })
}

#[derive(FromRow, Serialize)]
pub struct RbHintShowData {
    pub id: i32,
    pub title: Option<String>,
    pub title_hidden: bool,
    pub cooldown: i32,
    pub cost_id: Option<i32>,
    pub cost_amount: i64,
}

pub async fn get_hints_show_for_team(
    db_pool: &DbPool,
    team_id: i32,
    puzzle_id: i32,
) -> Result<Vec<RbHintShowData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbHintShowData,
        "SELECT h.id,
            CASE
                WHEN h.title_hidden
                    AND NOW() < GREATEST(tp.ctime_at, rp.release_at) + (h.cooldown * INTERVAL '1 second')
                THEN NULL
                ELSE h.title
            END AS \"title?\",
            h.title_hidden,
            h.cooldown, h.cost_id, h.cost_amount
        FROM rb_hint h
        JOIN rb_puzzle p ON p.id = h.puzzle_id
        JOIN rb_puzzle_effective_release rp ON rp.puzzle_id = p.id
        JOIN rb_team_puzzle tp ON tp.puzzle_id = h.puzzle_id AND tp.team_id = $1
        WHERE p.id = $2 AND tp.state >= 0 AND rp.release_at <= NOW()
        ORDER BY h.sort, h.id;",
        team_id,
        puzzle_id
    )
    .fetch_all(db_pool)
    .await?;

    Ok(result)
}

#[derive(FromRow, Serialize)]
pub struct RbHintTeamStateShowData {
    pub id: i32,
    pub title: String,
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
        "SELECT h.id, h.title, h.content, h.content_type
        FROM rb_hint h
        JOIN rb_team_hint th ON th.hint_id = h.id
        JOIN rb_puzzle p ON p.id = h.puzzle_id
        JOIN rb_puzzle_effective_release rp ON rp.puzzle_id = p.id
        JOIN rb_team_puzzle tp ON tp.puzzle_id = p.id AND tp.team_id = th.team_id
        WHERE th.team_id = $1
            AND h.puzzle_id = $2
            AND th.unlocked
            AND tp.state >= 0
            AND rp.release_at <= NOW();",
        team_id,
        puzzle_id
    )
    .fetch_all(db_pool)
    .await?;

    Ok(result)
}

#[derive(Serialize)]
pub struct RbPuzzleHintTeamData {
    pub data: Vec<RbHintShowData>,
    pub state: Vec<RbHintTeamStateShowData>,
}

pub async fn get_hints_view_for_team(
    db_pool: &DbPool,
    team_id: i32,
    puzzle_id: i32,
) -> Result<RbPuzzleHintTeamData, RbInternalError> {
    Ok(RbPuzzleHintTeamData {
        data: get_hints_show_for_team(db_pool, team_id, puzzle_id).await?,
        state: get_hints_team_state(db_pool, team_id, puzzle_id).await?,
    })
}

pub async fn sync_due_hints(
    db_pool: &DbPool,
    team_id: i32,
    puzzle_id: i32,
) -> Result<Option<OffsetDateTime>, RbInternalError> {
    let _ = get_hints_view_for_team(db_pool, team_id, puzzle_id).await?;

    let next_unlock_at = sqlx::query!(
        "SELECT MIN(GREATEST(tp.ctime_at, rp.release_at) + (h.cooldown * INTERVAL '1 second')) AS next_unlock_at
        FROM rb_hint h
        JOIN rb_puzzle p ON p.id = h.puzzle_id
        JOIN rb_puzzle_effective_release rp ON rp.puzzle_id = p.id
        JOIN rb_team_puzzle tp ON tp.puzzle_id = h.puzzle_id AND tp.team_id = $1
        WHERE h.puzzle_id = $2
            AND tp.state >= 0
            AND rp.release_at <= NOW()
            AND h.title_hidden
            AND GREATEST(tp.ctime_at, rp.release_at) + (h.cooldown * INTERVAL '1 second') > NOW();",
        team_id,
        puzzle_id
    )
    .fetch_one(db_pool)
    .await?
    .next_unlock_at;

    Ok(next_unlock_at)
}

pub enum PurchaseHintResult {
    Insufficient,
    Unavailable,
    Ok(RbHintTeamStateShowData),
}

pub async fn purchase_hint(
    app: &AppState,
    user_id: i32,
    hint_id: i32,
) -> Result<PurchaseHintResult, RbInternalError> {
    let info = sqlx::query!(
        "SELECT r.game_id, tm.team_id, t.name AS team_name, u.nickname AS user_nickname,
            h.puzzle_id, p.round_id, p.title AS puzzle_title,
            h.title AS hint_title, h.cost_id, h.cost_amount, h.backend_function
        FROM rb_hint h
        JOIN rb_puzzle p ON p.id = h.puzzle_id
        JOIN rb_round r ON r.id = p.round_id
        JOIN rb_puzzle_effective_release rp ON rp.puzzle_id = p.id
        JOIN rb_team_member tm ON tm.game_id = r.game_id
        JOIN rb_team t ON t.id = tm.team_id
        JOIN rb_user u ON u.id = tm.user_id
        JOIN rb_team_puzzle tp ON tp.puzzle_id = p.id AND tp.team_id = tm.team_id
        LEFT JOIN rb_team_hint th ON th.hint_id = h.id AND th.team_id = tm.team_id
        WHERE tm.user_id = $1 AND h.id = $2 AND tp.state >= 0
            AND rp.release_at <= NOW()
            AND NOT COALESCE(th.unlocked, FALSE)
            AND GREATEST(tp.ctime_at, rp.release_at) <= NOW() - (h.cooldown * INTERVAL '1 second');",
        user_id,
        hint_id
    )
    .fetch_optional(&app.db)
    .await?;

    if info.is_none() {
        return Ok(PurchaseHintResult::Unavailable);
    }
    let info = info.unwrap();

    let mut precheck_currency_event: Option<db::event_log::CurrencyEventData> = None;
    if info.cost_id.is_some() {
        let currency = sqlx::query!(
            r#"SELECT c.id AS "id!", c.slug AS "slug!", c.cname AS "name!", c.prec AS "prec!",
                LEAST(
                    tc.amount::NUMERIC + FLOOR(EXTRACT(EPOCH FROM (NOW() - tc.utime_at)) / 60) * (c.growth + tc.growth)::NUMERIC,
                    c.max_amount::NUMERIC
                )::BIGINT AS "before!"
            FROM rb_team_currency tc
            JOIN rb_currency c ON tc.currency_id = c.id
            WHERE tc.team_id = $1 AND c.id = $2
                AND ($3::BIGINT <= 0 OR LEAST(
                    tc.amount::NUMERIC + FLOOR(EXTRACT(EPOCH FROM (NOW() - tc.utime_at)) / 60) * (c.growth + tc.growth)::NUMERIC,
                    c.max_amount::NUMERIC
                )::BIGINT >= $3)"#,
            info.team_id,
            info.cost_id,
            info.cost_amount
        )
        .fetch_optional(&app.db)
        .await?;

        if let Some(currency) = currency {
            precheck_currency_event = Some(db::event_log::CurrencyEventData {
                id: currency.id,
                slug: currency.slug,
                name: currency.name,
                prec: currency.prec,
                before: currency.before,
                after: currency.before - info.cost_amount,
            });
        } else {
            return Ok(PurchaseHintResult::Insufficient);
        }
    }

    if let Some(function_name) = info.backend_function.as_deref() {
        let backend = db::puzzle_backend::get_backend(&app.db, info.puzzle_id)
            .await?
            .ok_or_else(|| RbInternalError::Other("hint backend function not found".to_string()))?;
        if !backend.enabled || !backend.export_enabled(function_name) {
            return Err(RbInternalError::Other(
                "hint backend function not callable".to_string(),
            ));
        }
        let currency = precheck_currency_event.as_ref().map(|currency| {
            json!({
                "id": currency.id,
                "slug": currency.slug,
                "name": currency.name,
                "prec": currency.prec,
                "before": currency.before,
                "after": currency.after,
                "delta": currency.delta(),
            })
        });
        crate::module::puzzle_backend_js::execute_hint_purchase(
            app,
            backend,
            function_name.to_string(),
            crate::module::puzzle_backend_js::HintPurchaseRuntimeContext {
                puzzle_id: info.puzzle_id,
                game_id: info.game_id,
                puzzle_title: info.puzzle_title.clone(),
                team_id: info.team_id,
                team_name: info.team_name.clone(),
                user_id,
                user_nickname: info.user_nickname.clone(),
                hint_id,
                hint_title: info.hint_title.clone(),
                cost_id: info.cost_id,
                cost_amount: info.cost_amount,
                currency: currency.unwrap_or(Value::Null),
            },
        )
        .await?;
    }

    let mut tx = app.db.begin().await?;
    let mut currency_event: Option<db::event_log::CurrencyEventData> = None;

    if info.cost_id.is_some() {
        let result = sqlx::query!(
            r#"WITH current AS (
                SELECT tc.team_id, c.id, c.slug, c.cname, c.prec,
                    LEAST(
                        tc.amount::NUMERIC + FLOOR(EXTRACT(EPOCH FROM (NOW() - tc.utime_at)) / 60) * (c.growth + tc.growth)::NUMERIC,
                        c.max_amount::NUMERIC
                    )::BIGINT AS current_amount
                FROM rb_team_currency tc
                JOIN rb_currency c ON tc.currency_id = c.id
                WHERE tc.team_id = $1 AND c.id = $2
                    AND ($3::BIGINT <= 0 OR LEAST(
                        tc.amount::NUMERIC + FLOOR(EXTRACT(EPOCH FROM (NOW() - tc.utime_at)) / 60) * (c.growth + tc.growth)::NUMERIC,
                        c.max_amount::NUMERIC
                    )::BIGINT >= $3)
                FOR UPDATE
            ), updated AS (
                UPDATE rb_team_currency tc
                SET utime_at = NOW(), amount = current.current_amount - $3
                FROM current
                WHERE tc.team_id = current.team_id AND tc.currency_id = current.id
                RETURNING current.id, current.slug, current.cname, current.prec,
                    current.current_amount, tc.amount
            )
            SELECT id AS "id!", slug AS "slug!", cname AS "name!", prec AS "prec!",
                current_amount AS "before!", amount AS "after!"
            FROM updated;"#,
            info.team_id, info.cost_id, info.cost_amount
        )
        .fetch_optional(&mut *tx)
        .await?;

        if let Some(currency) = result {
            currency_event = Some(db::event_log::CurrencyEventData {
                id: currency.id,
                slug: currency.slug,
                name: currency.name,
                prec: currency.prec,
                before: currency.before,
                after: currency.after,
            });
        } else {
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
        SELECT h.id, h.title, h.content, h.content_type
        FROM rb_hint h
        JOIN upserted u ON h.id = u.hint_id",
        info.team_id,
        hint_id
    )
    .fetch_one(&mut *tx)
    .await?;

    db::event_log::insert_tx(
        &mut tx,
        db::event_log::EventLogInput {
            event_type: "hint.purchased",
            event_scope: i16::from(db::event_log::EventScope::TeamActivity),
            severity: i16::from(db::event_log::EventSeverity::Info),
            game_id: Some(info.game_id),
            team_id: Some(info.team_id),
            user_id: Some(user_id),
            puzzle_id: Some(info.puzzle_id),
            round_id: Some(info.round_id),
            hint_id: Some(hint_id),
            currency_id: currency_event.as_ref().map(|currency| currency.id),
            delta_amount: currency_event.as_ref().map(|currency| currency.delta()),
            data: json!({
                "hint": {
                    "id": hint_id,
                    "title": info.hint_title,
                    "cost_id": info.cost_id,
                    "cost_amount": info.cost_amount
                },
                "puzzle": {
                    "id": info.puzzle_id,
                    "title": info.puzzle_title
                },
                "currency": currency_event.as_ref().map(|currency| json!({
                    "id": currency.id,
                    "slug": currency.slug,
                    "name": currency.name,
                    "prec": currency.prec
                })),
                "delta": currency_event.as_ref().map(|currency| currency.delta()),
                "before": currency_event.as_ref().map(|currency| currency.before),
                "after": currency_event.as_ref().map(|currency| currency.after)
            }),
            ..Default::default()
        },
    )
    .await?;

    db::cache::invalidate_team_hints(app, info.team_id, info.puzzle_id).await?;

    tx.commit().await?;
    Ok(PurchaseHintResult::Ok(result))
}

#[derive(Serialize)]
pub struct RbPuzzleAdminData {
    pub id: i32,
    pub game_id: i32,
    pub slug: Option<String>,
    pub sort: i32,
    pub title: String,
    pub ptype: i16,
    pub content: String,
    pub content_type: i16,
    pub judge: serde_json::Value,
    pub penalty: serde_json::Value,
    pub max_submit: Option<i32>,
    pub unlock_cond: String,
    pub release_phase_id: Option<i32>,
    #[serde(with = "crate::serde_helpers::serialize_option_offset_datetime")]
    pub immediate_release_at: Option<OffsetDateTime>,
    pub round_id: i32,
    pub ticket_enabled: bool,
    pub ticket_cooldown: i32,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub ctime_at: OffsetDateTime,
}

#[derive(Deserialize)]
pub struct RbPuzzleCreateData {
    pub slug: Option<String>,
    #[serde(default)]
    pub sort: i32,
    pub title: String,
    #[serde(default)]
    pub ptype: i16,
    pub content: String,
    #[serde(default)]
    pub content_type: i16,
    #[serde(default = "default_judge")]
    pub judge: serde_json::Value,
    #[serde(default = "default_penalty")]
    pub penalty: serde_json::Value,
    pub max_submit: Option<i32>,
    pub unlock_cond: String,
    pub release_phase_id: Option<i32>,
    #[serde(default)]
    pub release_immediately: bool,
    pub round_id: i32,
    #[serde(default = "default_ticket_enabled")]
    pub ticket_enabled: bool,
    #[serde(default)]
    pub ticket_cooldown: i32,
}

#[derive(Default, Deserialize)]
pub struct RbPuzzleUpdateData {
    #[serde(
        default,
        deserialize_with = "crate::serde_helpers::deserialize_nullable_string_patch"
    )]
    pub slug: Option<Option<String>>,
    pub sort: Option<i32>,
    pub title: Option<String>,
    pub ptype: Option<i16>,
    pub content: Option<String>,
    pub content_type: Option<i16>,
    pub judge: Option<serde_json::Value>,
    pub penalty: Option<serde_json::Value>,
    #[serde(
        default,
        deserialize_with = "crate::serde_helpers::deserialize_nullable_i32_patch"
    )]
    pub max_submit: Option<Option<i32>>,
    pub unlock_cond: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::serde_helpers::deserialize_nullable_i32_patch"
    )]
    pub release_phase_id: Option<Option<i32>>,
    pub release_immediately: Option<bool>,
    pub round_id: Option<i32>,
    pub ticket_enabled: Option<bool>,
    pub ticket_cooldown: Option<i32>,
}

fn default_judge() -> serde_json::Value {
    serde_json::json!({})
}

fn default_penalty() -> serde_json::Value {
    serde_json::json!([])
}

fn default_ticket_enabled() -> bool {
    true
}

pub async fn admin_list(
    pool: &DbPool,
    game_id: Option<i32>,
) -> Result<Vec<RbPuzzleAdminData>, RbInternalError> {
    let result = if let Some(game_id) = game_id {
        sqlx::query_as!(
            RbPuzzleAdminData,
            "SELECT p.id, r.game_id, p.slug, p.sort, p.title, p.ptype, p.content, p.content_type,
            p.judge, p.penalty, p.max_submit, p.unlock_cond, p.release_phase_id,
            p.immediate_release_at, p.round_id,
            p.ticket_enabled, p.ticket_cooldown, p.ctime_at
        FROM rb_puzzle p
        JOIN rb_round r ON r.id = p.round_id
        WHERE r.game_id = $1
        ORDER BY r.sort, r.id, (p.id IS DISTINCT FROM r.puzzle), p.sort, p.id;",
            game_id
        )
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as!(
            RbPuzzleAdminData,
            "SELECT p.id, r.game_id, p.slug, p.sort, p.title, p.ptype, p.content, p.content_type,
            p.judge, p.penalty, p.max_submit, p.unlock_cond, p.release_phase_id,
            p.immediate_release_at, p.round_id,
            p.ticket_enabled, p.ticket_cooldown, p.ctime_at
        FROM rb_puzzle p
        JOIN rb_round r ON r.id = p.round_id
        ORDER BY r.game_id, r.sort, r.id, (p.id IS DISTINCT FROM r.puzzle), p.sort, p.id;",
        )
        .fetch_all(pool)
        .await?
    };

    Ok(result)
}

pub async fn admin_get(
    pool: &DbPool,
    puzzle_id: i32,
) -> Result<Option<RbPuzzleAdminData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbPuzzleAdminData,
        "SELECT p.id, r.game_id, p.slug, p.sort, p.title, p.ptype, p.content, p.content_type,
            p.judge, p.penalty, p.max_submit, p.unlock_cond, p.release_phase_id,
            p.immediate_release_at, p.round_id,
            p.ticket_enabled, p.ticket_cooldown, p.ctime_at
        FROM rb_puzzle p
        JOIN rb_round r ON r.id = p.round_id
        WHERE p.id = $1;",
        puzzle_id
    )
    .fetch_optional(pool)
    .await?;

    Ok(result)
}

async fn clear_immediate_release_events_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    puzzle_ids: &[i32],
) -> Result<(), RbInternalError> {
    sqlx::query!(
        "DELETE FROM rb_release_event_puzzle rep
        USING rb_release_event re
        WHERE rep.event_id = re.id AND re.event_type = 1
            AND rep.puzzle_id = ANY($1);",
        puzzle_ids
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn create_immediate_release_event_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
    puzzle_ids: &[i32],
    occurred_at: OffsetDateTime,
) -> Result<(), RbInternalError> {
    let event_id = sqlx::query_scalar!(
        "INSERT INTO rb_release_event (game_id, event_type, occurred_at)
        VALUES ($1, 1, $2) RETURNING id;",
        game_id,
        occurred_at
    )
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query!(
        "INSERT INTO rb_release_event_puzzle (event_id, puzzle_id)
        SELECT $1, p.id FROM rb_puzzle p
        WHERE p.game_id = $2 AND p.id = ANY($3);",
        event_id,
        game_id,
        puzzle_ids
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "INSERT INTO rb_release_event_puzzle_team (event_id, puzzle_id, team_id)
        SELECT $1, tp.puzzle_id, tp.team_id
        FROM rb_team_puzzle tp
        JOIN rb_puzzle p ON p.id = tp.puzzle_id
        WHERE p.game_id = $2 AND tp.puzzle_id = ANY($3) AND tp.state >= 0;",
        event_id,
        game_id,
        puzzle_ids
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn admin_create(
    pool: &DbPool,
    data: &RbPuzzleCreateData,
) -> Result<Option<RbPuzzleAdminData>, RbInternalError> {
    let mut tx = pool.begin().await?;
    let result = sqlx::query_as!(
        RbPuzzleAdminData,
        "INSERT INTO rb_puzzle (
            slug, sort, title, ptype, content, content_type, judge, penalty,
            max_submit, unlock_cond, release_phase_id, immediate_release_at, round_id,
            ticket_enabled, ticket_cooldown
        )
        SELECT $2, $3, $4, $5, $6, $7, $8, $9, $10, $11,
            CASE WHEN $13 THEN NULL ELSE $12::INT END,
            CASE WHEN $13 THEN NOW() ELSE NULL END,
            r.id, $14, $15
        FROM rb_round r
        WHERE r.id = $1
            AND NOT ($13 AND $12::INT IS NOT NULL)
            AND ($12::INT IS NULL OR EXISTS (
                SELECT 1 FROM rb_release_phase rp
                WHERE rp.id = $12::INT AND rp.game_id = r.game_id
                    AND rp.release_at > NOW()
                    AND NOT EXISTS (SELECT 1 FROM rb_release_event re WHERE re.phase_id = rp.id)
            ))
        RETURNING id, game_id,
            slug, sort, title, ptype, content, content_type, judge, penalty,
            max_submit, unlock_cond, release_phase_id, immediate_release_at, round_id,
            ticket_enabled, ticket_cooldown, ctime_at;",
        data.round_id,
        data.slug,
        data.sort,
        data.title,
        data.ptype,
        data.content,
        data.content_type,
        data.judge,
        data.penalty,
        data.max_submit,
        data.unlock_cond,
        data.release_phase_id,
        data.release_immediately,
        data.ticket_enabled,
        data.ticket_cooldown
    )
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(puzzle) = &result
        && let Some(released_at) = puzzle.immediate_release_at
    {
        create_immediate_release_event_tx(&mut tx, puzzle.game_id, &[puzzle.id], released_at)
            .await?;
    }
    tx.commit().await?;
    Ok(result)
}

pub async fn admin_update(
    pool: &DbPool,
    puzzle_id: i32,
    data: &RbPuzzleUpdateData,
) -> Result<Option<RbPuzzleAdminData>, RbInternalError> {
    let max_submit_is_set = data.max_submit.is_some();
    let max_submit = data.max_submit.flatten();
    let slug_is_set = data.slug.is_some();
    let slug = data.slug.clone().flatten();
    let release_phase_is_set = data.release_phase_id.is_some();
    let release_phase_id = data.release_phase_id.flatten();
    let release_immediately = data.release_immediately == Some(true);
    let release_is_set = release_phase_is_set || release_immediately;

    let mut tx = pool.begin().await?;
    if release_is_set {
        clear_immediate_release_events_tx(&mut tx, &[puzzle_id]).await?;
    }
    let result = sqlx::query_as!(
        RbPuzzleAdminData,
        "UPDATE rb_puzzle p
        SET slug = CASE WHEN $2 THEN $3 ELSE p.slug END,
            sort = CASE
                WHEN EXISTS (SELECT 1 FROM rb_round owner_round WHERE owner_round.puzzle = p.id) THEN p.sort
                ELSE COALESCE($4, p.sort)
            END,
            title = COALESCE($5, p.title),
            ptype = COALESCE($6, p.ptype),
            content = COALESCE($7, p.content),
            content_type = COALESCE($8, p.content_type),
            judge = COALESCE($9, p.judge),
            penalty = COALESCE($10, p.penalty),
            max_submit = CASE WHEN $11 THEN $12 ELSE p.max_submit END,
            unlock_cond = COALESCE($13, p.unlock_cond),
            release_phase_id = CASE
                WHEN $16 THEN NULL
                WHEN $14 THEN $15
                ELSE p.release_phase_id
            END,
            immediate_release_at = CASE
                WHEN $16 THEN NOW()
                WHEN $14 THEN NULL
                ELSE p.immediate_release_at
            END,
            round_id = COALESCE((
                SELECT r.id FROM rb_round r WHERE r.id = $17::INT
            ), p.round_id),
            ticket_enabled = COALESCE($18, p.ticket_enabled),
            ticket_cooldown = COALESCE($19, p.ticket_cooldown)
        WHERE p.id = $1
            AND NOT ($16 AND $14)
            AND (NOT $14 OR $15::INT IS NULL OR EXISTS (
                SELECT 1 FROM rb_release_phase target_phase
                WHERE target_phase.id = $15::INT AND target_phase.game_id = p.game_id
                    AND target_phase.release_at > NOW()
                    AND NOT EXISTS (SELECT 1 FROM rb_release_event target_event WHERE target_event.phase_id = target_phase.id)
            ))
            AND ($17::INT IS NULL OR EXISTS (
                SELECT 1 FROM rb_round target_round WHERE target_round.id = $17::INT
            ))
            AND ($17::INT IS NULL OR NOT EXISTS (
                SELECT 1 FROM rb_round owner_round
                WHERE owner_round.puzzle = p.id AND owner_round.id IS DISTINCT FROM $17::INT
            ))
        RETURNING p.id, p.game_id,
            p.slug, p.sort, p.title, p.ptype, p.content, p.content_type,
            p.judge, p.penalty, p.max_submit, p.unlock_cond, p.release_phase_id,
            p.immediate_release_at, p.round_id,
            p.ticket_enabled, p.ticket_cooldown, p.ctime_at;",
        puzzle_id,
        slug_is_set,
        slug,
        data.sort,
        data.title,
        data.ptype,
        data.content,
        data.content_type,
        data.judge,
        data.penalty,
        max_submit_is_set,
        max_submit,
        data.unlock_cond,
        release_phase_is_set,
        release_phase_id,
        release_immediately,
        data.round_id,
        data.ticket_enabled,
        data.ticket_cooldown
    )
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(puzzle) = &result
        && release_immediately
        && let Some(released_at) = puzzle.immediate_release_at
    {
        create_immediate_release_event_tx(&mut tx, puzzle.game_id, &[puzzle.id], released_at)
            .await?;
    }
    tx.commit().await?;
    Ok(result)
}

pub async fn admin_batch_update_release_phase(
    pool: &DbPool,
    game_id: i32,
    puzzle_ids: &[i32],
    release_phase_id: Option<i32>,
    release_immediately: bool,
) -> Result<Option<Vec<RbPuzzleAdminData>>, RbInternalError> {
    let mut tx = pool.begin().await?;
    clear_immediate_release_events_tx(&mut tx, puzzle_ids).await?;
    let puzzles = sqlx::query_as!(
        RbPuzzleAdminData,
        "UPDATE rb_puzzle p
        SET release_phase_id = CASE WHEN $4 THEN NULL ELSE $3 END,
            immediate_release_at = CASE WHEN $4 THEN NOW() ELSE NULL END
        WHERE p.game_id = $1 AND p.id = ANY($2)
            AND NOT ($4 AND $3::INT IS NOT NULL)
            AND ($3::INT IS NULL OR EXISTS (
                SELECT 1 FROM rb_release_phase target
                WHERE target.id = $3::INT AND target.game_id = p.game_id
                    AND target.release_at > NOW()
                    AND NOT EXISTS (
                        SELECT 1 FROM rb_release_event target_event
                        WHERE target_event.phase_id = target.id
                    )
            ))
        RETURNING p.id, p.game_id, p.slug, p.sort, p.title, p.ptype, p.content,
            p.content_type, p.judge, p.penalty, p.max_submit, p.unlock_cond,
            p.release_phase_id, p.immediate_release_at, p.round_id,
            p.ticket_enabled, p.ticket_cooldown, p.ctime_at;",
        game_id,
        puzzle_ids,
        release_phase_id,
        release_immediately
    )
    .fetch_all(&mut *tx)
    .await?;

    if puzzles.len() != puzzle_ids.len() {
        return Ok(None);
    }

    if release_immediately
        && let Some(released_at) = puzzles
            .first()
            .and_then(|puzzle| puzzle.immediate_release_at)
    {
        create_immediate_release_event_tx(&mut tx, game_id, puzzle_ids, released_at).await?;
    }

    tx.commit().await?;
    Ok(Some(puzzles))
}

pub async fn admin_delete(pool: &DbPool, puzzle_id: i32) -> Result<bool, RbInternalError> {
    let result = sqlx::query!(
        "DELETE FROM rb_puzzle
        WHERE id = $1;",
        puzzle_id
    )
    .execute(pool)
    .await?;

    Ok(result.rows_affected() > 0)
}

#[derive(FromRow, Serialize)]
pub struct RbHintAdminData {
    pub id: i32,
    pub sort: i32,
    pub title: String,
    pub title_hidden: bool,
    pub content: String,
    pub content_type: i16,
    pub cooldown: i32,
    pub cost_id: Option<i32>,
    pub cost_amount: i64,
    pub backend_function: Option<String>,
    pub puzzle_id: i32,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub ctime_at: OffsetDateTime,
}

#[derive(Deserialize)]
pub struct RbHintCreateData {
    #[serde(default)]
    pub sort: i32,
    pub title: String,
    #[serde(default)]
    pub title_hidden: bool,
    pub content: String,
    #[serde(default)]
    pub content_type: i16,
    #[serde(default)]
    pub cooldown: i32,
    pub cost_id: Option<i32>,
    #[serde(default)]
    pub cost_amount: i64,
    pub backend_function: Option<String>,
    pub puzzle_id: i32,
}

#[derive(Default, Deserialize)]
pub struct RbHintUpdateData {
    pub sort: Option<i32>,
    pub title: Option<String>,
    pub title_hidden: Option<bool>,
    pub content: Option<String>,
    pub content_type: Option<i16>,
    pub cooldown: Option<i32>,
    #[serde(
        default,
        deserialize_with = "crate::serde_helpers::deserialize_nullable_i32_patch"
    )]
    pub cost_id: Option<Option<i32>>,
    pub cost_amount: Option<i64>,
    #[serde(
        default,
        deserialize_with = "crate::serde_helpers::deserialize_nullable_string_patch"
    )]
    pub backend_function: Option<Option<String>>,
    pub puzzle_id: Option<i32>,
}

pub async fn admin_list_hints(
    pool: &DbPool,
    puzzle_id: Option<i32>,
) -> Result<Vec<RbHintAdminData>, RbInternalError> {
    let result = if let Some(puzzle_id) = puzzle_id {
        sqlx::query_as!(
            RbHintAdminData,
            "SELECT id, sort, title, title_hidden, content, content_type, cooldown, cost_id,
                cost_amount, backend_function, puzzle_id, ctime_at
            FROM rb_hint
            WHERE puzzle_id = $1
            ORDER BY sort, id;",
            puzzle_id
        )
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as!(
            RbHintAdminData,
            "SELECT id, sort, title, title_hidden, content, content_type, cooldown, cost_id,
                cost_amount, backend_function, puzzle_id, ctime_at
            FROM rb_hint
            ORDER BY puzzle_id, sort, id;"
        )
        .fetch_all(pool)
        .await?
    };

    Ok(result)
}

pub async fn admin_get_hint(
    pool: &DbPool,
    hint_id: i32,
) -> Result<Option<RbHintAdminData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbHintAdminData,
        "SELECT id, sort, title, title_hidden, content, content_type, cooldown, cost_id,
            cost_amount, backend_function, puzzle_id, ctime_at
        FROM rb_hint
        WHERE id = $1;",
        hint_id
    )
    .fetch_optional(pool)
    .await?;

    Ok(result)
}

pub async fn admin_create_hint(
    pool: &DbPool,
    data: &RbHintCreateData,
) -> Result<Option<RbHintAdminData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbHintAdminData,
        "INSERT INTO rb_hint (
            sort, title, title_hidden, content, content_type, cooldown, cost_id, cost_amount,
            backend_function, puzzle_id
        )
        SELECT $2, $3, $4, $5, $6, $7, $8, $9, $10, p.id
        FROM rb_puzzle p
        WHERE p.id = $1
        RETURNING id, sort, title, title_hidden, content, content_type, cooldown, cost_id,
            cost_amount, backend_function, puzzle_id, ctime_at;",
        data.puzzle_id,
        data.sort,
        data.title,
        data.title_hidden,
        data.content,
        data.content_type,
        data.cooldown,
        data.cost_id,
        data.cost_amount,
        data.backend_function,
    )
    .fetch_optional(pool)
    .await?;

    Ok(result)
}

pub async fn admin_update_hint(
    pool: &DbPool,
    hint_id: i32,
    data: &RbHintUpdateData,
) -> Result<Option<RbHintAdminData>, RbInternalError> {
    let cost_id_is_set = data.cost_id.is_some();
    let cost_id = data.cost_id.flatten();
    let backend_function_is_set = data.backend_function.is_some();
    let backend_function = data.backend_function.clone().flatten();

    let result = sqlx::query_as!(
        RbHintAdminData,
        "UPDATE rb_hint h
        SET sort = COALESCE($2, h.sort),
            title = COALESCE($3, h.title),
            title_hidden = COALESCE($4, h.title_hidden),
            content = COALESCE($5, h.content),
            content_type = COALESCE($6, h.content_type),
            cooldown = COALESCE($7, h.cooldown),
            cost_id = CASE WHEN $8 THEN $9 ELSE h.cost_id END,
            cost_amount = CASE
                WHEN $8 AND $9::INT IS NULL THEN 0
                ELSE COALESCE($10, h.cost_amount)
            END,
            backend_function = CASE WHEN $11 THEN $12 ELSE h.backend_function END,
            puzzle_id = COALESCE((
                SELECT p.id FROM rb_puzzle p WHERE p.id = $13::INT
            ), h.puzzle_id)
        WHERE h.id = $1
            AND ($13::INT IS NULL OR EXISTS (
                SELECT 1 FROM rb_puzzle p WHERE p.id = $13::INT
            ))
        RETURNING id, sort, title, title_hidden, content, content_type, cooldown, cost_id,
            cost_amount, backend_function, puzzle_id, ctime_at;",
        hint_id,
        data.sort,
        data.title,
        data.title_hidden,
        data.content,
        data.content_type,
        data.cooldown,
        cost_id_is_set,
        cost_id,
        data.cost_amount,
        backend_function_is_set,
        backend_function,
        data.puzzle_id
    )
    .fetch_optional(pool)
    .await?;

    Ok(result)
}

pub async fn admin_delete_hint(pool: &DbPool, hint_id: i32) -> Result<bool, RbInternalError> {
    let result = sqlx::query!(
        "DELETE FROM rb_hint
        WHERE id = $1;",
        hint_id
    )
    .execute(pool)
    .await?;

    Ok(result.rows_affected() > 0)
}
