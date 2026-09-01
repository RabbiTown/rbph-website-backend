use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgConnection, prelude::FromRow};
use time::OffsetDateTime;

use crate::{
    AppState, DbPool, KvPool,
    db::{self, game::GameUserInfo},
    error::RbInternalError,
    expr::{self, types::PuzzleStates},
    extractor::auth::AuthUser,
    game::{
        self,
        judge::{JudgeResult, normalize_answer},
    },
    model::game::{
        RbContentType, RbJudgeAction, RbPuzzlePenaltyType, RbPuzzleType, RbTeamPuzzleState,
    },
    model::user::RbUserRole,
};

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

pub async fn get_puzzle_round(
    db_pool: &DbPool,
    puzzle_id: i32,
) -> Result<Option<i32>, RbInternalError> {
    let result = sqlx::query_scalar::<_, i32>("SELECT round_id FROM rb_puzzle WHERE id = $1;")
        .bind(puzzle_id)
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
    pub round: RbPuzzleShowRoundData,
    pub game_id: i32,
    pub submission_enabled: bool,
    pub submit_requirements: Vec<PuzzleSubmitRequirementShowData>,
    pub announcements: Vec<db::anmt::RbAnnouncementShowData>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PuzzleSubmitRequirement {
    CurrencyMinimum { currency_id: i32, minimum: i64 },
}

fn parse_submit_requirements(
    value: serde_json::Value,
) -> Result<Vec<PuzzleSubmitRequirement>, serde_json::Error> {
    serde_json::from_value(value)
}

#[derive(Serialize)]
pub struct PuzzleSubmitRequirementShowData {
    #[serde(rename = "type")]
    pub requirement_type: &'static str,
    pub currency_id: i32,
    pub currency_name: String,
    pub currency_prec: i32,
    pub minimum: i64,
}

struct PuzzleForTeamRow {
    id: i32,
    slug: Option<String>,
    title: String,
    ptype: i16,
    judge: Value,
    round_id: i32,
    round_slug: Option<String>,
    round_title: String,
    game_id: i32,
    submit_requirements: Value,
    state: i16,
    max_submit: Option<i32>,
    submit_count: i64,
    answers: Option<Vec<String>>,
    utime_at: OffsetDateTime,
    cooldown_till: Option<OffsetDateTime>,
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

#[derive(Serialize)]
pub struct RbPuzzleForTeamData {
    data: RbPuzzleShowData,
    state: RbPuzzleTeamStateShowData,
}

async fn load_submit_requirements(
    db_pool: &DbPool,
    game_id: i32,
    requirements: Vec<PuzzleSubmitRequirement>,
) -> Result<Vec<PuzzleSubmitRequirementShowData>, RbInternalError> {
    if requirements.is_empty() {
        return Ok(Vec::new());
    }

    let currency_ids = requirements
        .iter()
        .map(|requirement| match requirement {
            PuzzleSubmitRequirement::CurrencyMinimum { currency_id, .. } => *currency_id,
        })
        .collect::<Vec<_>>();
    let currencies = sqlx::query!(
        "SELECT id, cname, prec
         FROM rb_currency
         WHERE game_id = $1 AND id = ANY($2)",
        game_id,
        &currency_ids
    )
    .fetch_all(db_pool)
    .await?
    .into_iter()
    .map(|currency| (currency.id, (currency.cname, currency.prec)))
    .collect::<HashMap<_, _>>();

    Ok(requirements
        .into_iter()
        .filter_map(|requirement| match requirement {
            PuzzleSubmitRequirement::CurrencyMinimum {
                currency_id,
                minimum,
            } => currencies
                .get(&currency_id)
                .map(
                    |(currency_name, currency_prec)| PuzzleSubmitRequirementShowData {
                        requirement_type: "currency_minimum",
                        currency_id,
                        currency_name: currency_name.clone(),
                        currency_prec: *currency_prec,
                        minimum,
                    },
                ),
        })
        .collect())
}

pub async fn get_puzzle_for_team(
    db_pool: &DbPool,
    team_id: i32,
    puzzle_id: i32,
) -> Result<Option<RbPuzzleForTeamData>, RbInternalError> {
    let row = sqlx::query_as!(
        PuzzleForTeamRow,
        r#"SELECT p.id, p.slug, p.title, p.ptype, p.judge, p.submit_requirements,
                  p.round_id, r.slug AS round_slug, r.title AS round_title, r.game_id,
                  tp.state, tp.max_submit + p.max_submit AS max_submit,
                  COALESCE(submission.submit_count, 0) AS "submit_count!",
                  submission.answers,
                  GREATEST(tp.ctime_at, release.release_at) AS "utime_at!",
                  tp.cooldown_till
           FROM rb_puzzle p
           JOIN rb_round r ON r.id = p.round_id AND r.puzzle IS DISTINCT FROM p.id
           JOIN rb_team_puzzle tp
             ON tp.puzzle_id = p.id AND tp.team_id = $1 AND tp.state >= 0
           JOIN rb_team t ON t.id = tp.team_id AND NOT t.is_banned
           JOIN rb_puzzle_effective_release release
             ON release.puzzle_id = p.id AND release.release_at <= NOW()
           LEFT JOIN LATERAL (
               SELECT COUNT(*) FILTER (
                          WHERE s.saction = 0 AND NOT s.ignored
                      )::BIGINT AS submit_count,
                      ARRAY_AGG(DISTINCT s.real_answer) FILTER (
                          WHERE s.saction = 1 AND s.real_answer IS NOT NULL
                      ) AS answers
               FROM rb_submission s
               WHERE s.team_id = tp.team_id AND s.puzzle_id = p.id
           ) submission ON TRUE
           WHERE p.id = $2"#,
        team_id,
        puzzle_id,
    )
    .fetch_optional(db_pool)
    .await?;

    let Some(row) = row else {
        return Ok(None);
    };
    let requirements =
        parse_submit_requirements(row.submit_requirements.clone()).unwrap_or_default();
    let (submit_requirements, announcements) = tokio::try_join!(
        load_submit_requirements(db_pool, row.game_id, requirements),
        db::anmt::list_for_team_puzzle(db_pool, team_id, puzzle_id),
    )?;

    Ok(Some(RbPuzzleForTeamData {
        data: RbPuzzleShowData {
            id: row.id,
            slug: row.slug,
            title: row.title,
            ptype: row.ptype.into(),
            round: RbPuzzleShowRoundData {
                id: row.round_id,
                slug: row.round_slug,
                title: row.round_title,
            },
            game_id: row.game_id,
            submission_enabled: game::judge::value_to_judge(row.judge)
                .is_ok_and(|rules| !rules.is_empty()),
            submit_requirements,
            announcements,
        },
        state: RbPuzzleTeamStateShowData {
            state: row.state.into(),
            max_submit: row.max_submit,
            submit_count: row.submit_count,
            answers: row.answers.unwrap_or_default(),
            utime_at: row.utime_at,
            cooldown_till: row.cooldown_till,
        },
    }))
}

#[derive(Clone, Serialize)]
pub struct SubmitStateUpdate {
    pub state: Option<RbPuzzleTeamStateShowData>,
    pub currency: Vec<db::team::RbCurrencyShowData>,
    pub currency_penalty: Vec<CurrencyPenaltyShowData>,
    pub content_changed: bool,
}

#[derive(Clone)]
pub struct SubmitStateBox(pub Box<SubmitStateUpdate>);

#[derive(Clone, Serialize)]
pub struct BackendSubmissionInput {
    pub user_answer: String,
    pub norm_answer: Option<String>,
    pub action: RbJudgeAction,
    pub result: Option<String>,
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
    #[serde(rename = "action")]
    pub saction: RbJudgeAction,
    #[serde(rename = "result")]
    pub sresult: Option<String>,
    pub real_answer: Option<String>,
    pub ignored: bool,
    #[serde(
        rename = "createdAt",
        with = "crate::serde_helpers::serialize_offset_datetime"
    )]
    pub ctime_at: OffsetDateTime,
}

pub async fn insert_backend_submission(
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
        i16::from(data.action),
        data.result,
        data.real_answer,
        data.ignored
    )
    .fetch_one(pool)
    .await?;

    Ok(row)
}

pub async fn add_backend_submission(
    app: &AppState,
    team_id: i32,
    user_id: i32,
    puzzle_id: i32,
    data: &BackendSubmissionInput,
) -> Result<BackendSubmissionShowData, RbInternalError> {
    let row = insert_backend_submission(&app.db, team_id, user_id, puzzle_id, data).await?;
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

    sqlx::query_scalar!("SELECT id FROM rb_team WHERE id = $1 FOR UPDATE", team_id)
        .fetch_optional(&mut *tx)
        .await?;

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
        db::content::mark_team_dirty_conn(&mut tx, team_id).await?;
        db::ticket::close_puzzle_tickets_on_solve_conn(&mut tx, team_id, puzzle_id, user_id)
            .await?;
    }

    tx.commit().await?;

    if solved {
        db::board::LEADER_BOARD_CACHE
            .update_team(&app.db, team_id, true)
            .await?;
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

#[derive(Serialize)]
pub struct StaffPuzzleHintStatus {
    pub id: i32,
    pub title: String,
    pub cooldown: i32,
    pub cost_id: Option<i32>,
    pub cost_name: Option<String>,
    pub cost_prec: Option<i32>,
    pub cost_amount: i64,
    pub enabled: bool,
    #[serde(with = "crate::serde_helpers::serialize_option_offset_datetime")]
    pub available_at: Option<OffsetDateTime>,
    pub unlocked: bool,
    #[serde(with = "crate::serde_helpers::serialize_option_offset_datetime")]
    pub unlocked_at: Option<OffsetDateTime>,
}

#[derive(Serialize)]
pub struct StaffPuzzleTeamStatus {
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub server_time: OffsetDateTime,
    pub state: RbTeamPuzzleState,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub unlock_at: OffsetDateTime,
    #[serde(with = "crate::serde_helpers::serialize_option_offset_datetime")]
    pub solve_at: Option<OffsetDateTime>,
    #[serde(with = "crate::serde_helpers::serialize_option_offset_datetime")]
    pub cooldown_till: Option<OffsetDateTime>,
    pub cooldown_active: bool,
    pub submission_enabled: bool,
    pub submit_requirements_met: bool,
    pub can_submit: bool,
    pub team_banned: bool,
    pub max_submit: Option<i32>,
    pub submit_count: i64,
    pub remaining_submit: Option<i64>,
    pub hints: Vec<StaffPuzzleHintStatus>,
}

pub async fn get_staff_puzzle_team_status(
    pool: &DbPool,
    game_id: i32,
    team_id: i32,
    puzzle_id: i32,
) -> Result<Option<StaffPuzzleTeamStatus>, RbInternalError> {
    refresh_team_hint_enablements(pool, team_id, Some(puzzle_id)).await?;
    let row = sqlx::query!(
        "SELECT NOW() AS \"server_time!\", tp.state,
            GREATEST(tp.ctime_at, rp.release_at) AS \"unlock_at!\",
            tp.solve_at, tp.cooldown_till,
            tp.cooldown_till IS NOT NULL AND tp.cooldown_till > NOW() AS \"cooldown_active!\",
            tp.max_submit + p.max_submit AS max_submit,
            COUNT(fs.id) AS submit_count,
            p.judge, p.submit_requirements,
            t.is_banned
        FROM rb_team_puzzle tp
        JOIN rb_team t ON t.id = tp.team_id
        JOIN rb_puzzle p ON p.id = tp.puzzle_id
        JOIN rb_puzzle_effective_release rp ON rp.puzzle_id = p.id
        LEFT JOIN rb_submission fs ON fs.puzzle_id = tp.puzzle_id
            AND fs.team_id = tp.team_id
            AND fs.saction = 0
            AND NOT fs.ignored
        WHERE p.game_id = $1 AND t.game_id = $1
            AND tp.team_id = $2 AND tp.puzzle_id = $3
        GROUP BY tp.state, GREATEST(tp.ctime_at, rp.release_at),
            tp.solve_at, tp.cooldown_till, tp.max_submit, p.max_submit,
            p.judge, p.submit_requirements, t.is_banned;",
        game_id,
        team_id,
        puzzle_id,
    )
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else {
        return Ok(None);
    };

    let submit_count = row.submit_count.unwrap_or(0);
    let remaining_submit = row
        .max_submit
        .map(|maximum| (i64::from(maximum) - submit_count).max(0));
    let submission_enabled =
        game::judge::value_to_judge(row.judge).is_ok_and(|rules| !rules.is_empty());
    let submit_requirements_met = match parse_submit_requirements(row.submit_requirements) {
        Ok(requirements) => {
            let mut met = true;
            for requirement in requirements {
                let PuzzleSubmitRequirement::CurrencyMinimum {
                    currency_id,
                    minimum,
                } = requirement;
                let current = sqlx::query_scalar!(
                    r#"SELECT CASE WHEN gf.state = 1 THEN
                                GREATEST(LEAST(tc.amount::NUMERIC, 0::NUMERIC), LEAST(
                                    tc.amount::NUMERIC
                                        + FLOOR(EXTRACT(EPOCH FROM (NOW() - tc.utime_at)) / 60)
                                            * (c.growth + tc.growth)::NUMERIC,
                                    c.max_amount::NUMERIC
                                ))::BIGINT
                            ELSE tc.amount END AS "current_amount!"
                        FROM rb_team_currency tc
                        JOIN rb_currency c ON c.id = tc.currency_id
                        JOIN rb_game_feature gf
                            ON gf.game_id = c.game_id AND gf.feature_type = 4
                        WHERE tc.team_id = $1
                            AND tc.currency_id = $2
                            AND c.game_id = $3"#,
                    team_id,
                    currency_id,
                    game_id,
                )
                .fetch_optional(pool)
                .await?;
                if current.is_none_or(|amount| amount < minimum) {
                    met = false;
                    break;
                }
            }
            met
        }
        Err(_) => false,
    };
    let state: RbTeamPuzzleState = row.state.into();
    let can_submit = state.accessible()
        && !row.is_banned
        && !row.cooldown_active
        && remaining_submit != Some(0)
        && submission_enabled
        && submit_requirements_met;

    let hints = sqlx::query!(
        "SELECT h.id, h.title, h.cooldown, h.cost_id,
            c.cname AS \"cost_name?\", c.prec AS \"cost_prec?\", h.cost_amount,
            (h.enable_cond IS NULL OR enabled.hint_id IS NOT NULL
                OR COALESCE(th.unlocked, FALSE)) AS \"enabled!\",
            CASE
                WHEN h.enable_cond IS NOT NULL
                    AND enabled.hint_id IS NULL
                    AND NOT COALESCE(th.unlocked, FALSE)
                THEN NULL
                ELSE (CASE
                    WHEN h.cooldown_after_enable
                        THEN COALESCE(enabled.enabled_at, th.utime_at)
                    ELSE GREATEST(tp.ctime_at, rp.release_at)
                END) + (h.cooldown::BIGINT * INTERVAL '1 second')
            END AS available_at,
            COALESCE(th.unlocked, FALSE) AS \"unlocked!\",
            CASE WHEN th.unlocked THEN th.utime_at ELSE NULL END AS unlocked_at
        FROM rb_hint h
        JOIN rb_puzzle p ON p.id = h.puzzle_id
        JOIN rb_puzzle_effective_release rp ON rp.puzzle_id = p.id
        JOIN rb_team_puzzle tp ON tp.puzzle_id = p.id AND tp.team_id = $2
        LEFT JOIN rb_team_hint th ON th.hint_id = h.id AND th.team_id = $2
        LEFT JOIN rb_team_hint_enable enabled
            ON enabled.hint_id = h.id AND enabled.team_id = $2
        LEFT JOIN rb_currency c ON c.id = h.cost_id
        WHERE p.game_id = $1 AND p.id = $3
        ORDER BY h.sort, h.id;",
        game_id,
        team_id,
        puzzle_id,
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|hint| StaffPuzzleHintStatus {
        id: hint.id,
        title: hint.title,
        cooldown: hint.cooldown,
        cost_id: hint.cost_id,
        cost_name: hint.cost_name,
        cost_prec: hint.cost_prec,
        cost_amount: hint.cost_amount,
        enabled: hint.enabled,
        available_at: hint.available_at,
        unlocked: hint.unlocked,
        unlocked_at: hint.unlocked_at,
    })
    .collect();

    Ok(Some(StaffPuzzleTeamStatus {
        server_time: row.server_time,
        state,
        unlock_at: row.unlock_at,
        solve_at: row.solve_at,
        cooldown_till: row.cooldown_till,
        cooldown_active: row.cooldown_active,
        submission_enabled,
        submit_requirements_met,
        can_submit,
        team_banned: row.is_banned,
        max_submit: row.max_submit,
        submit_count,
        remaining_submit,
        hints,
    }))
}

#[derive(Serialize)]
pub struct StaffPuzzleHintContent {
    pub id: i32,
    pub title: String,
    pub content: String,
    pub content_type: RbContentType,
}

pub async fn get_staff_puzzle_hint_content(
    pool: &DbPool,
    game_id: i32,
    team_id: i32,
    puzzle_id: i32,
    hint_id: i32,
) -> Result<Option<StaffPuzzleHintContent>, RbInternalError> {
    let result = sqlx::query_as!(
        StaffPuzzleHintContent,
        "SELECT h.id, h.title, h.content, h.content_type
        FROM rb_hint h
        JOIN rb_puzzle p ON p.id = h.puzzle_id
        JOIN rb_team_puzzle tp ON tp.puzzle_id = p.id AND tp.team_id = $2
        JOIN rb_team t ON t.id = tp.team_id
        WHERE p.game_id = $1 AND t.game_id = $1
            AND p.id = $3 AND h.id = $4;",
        game_id,
        team_id,
        puzzle_id,
        hint_id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(result)
}

#[derive(Serialize)]
pub struct StaffPuzzleSubmission {
    pub id: i32,
    pub user_id: i32,
    pub user_name: String,
    pub user_answer: String,
    pub norm_answer: String,
    pub saction: RbJudgeAction,
    pub sresult: Option<String>,
    pub real_answer: Option<String>,
    pub ignored: bool,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub ctime_at: OffsetDateTime,
}

#[derive(Serialize)]
pub struct StaffPuzzleSubmissionPage {
    pub data: Vec<StaffPuzzleSubmission>,
    pub total: i64,
}

pub async fn get_staff_puzzle_submissions(
    pool: &DbPool,
    game_id: i32,
    team_id: i32,
    puzzle_id: i32,
    page: i64,
    limit: i64,
    only_ok: bool,
) -> Result<Option<StaffPuzzleSubmissionPage>, RbInternalError> {
    let exists = sqlx::query_scalar!(
        "SELECT EXISTS(
            SELECT 1
            FROM rb_team_puzzle tp
            JOIN rb_team t ON t.id = tp.team_id
            JOIN rb_puzzle p ON p.id = tp.puzzle_id
            WHERE p.game_id = $1 AND t.game_id = $1
                AND tp.team_id = $2 AND tp.puzzle_id = $3
        ) AS \"exists!\";",
        game_id,
        team_id,
        puzzle_id,
    )
    .fetch_one(pool)
    .await?;
    if !exists {
        return Ok(None);
    }

    let rows = sqlx::query!(
        "SELECT s.id, s.user_id, u.nickname AS user_name,
            s.user_answer, s.norm_answer, s.saction, s.sresult,
            s.real_answer, s.ignored, s.ctime_at,
            COUNT(*) OVER() AS total
        FROM rb_submission s
        JOIN rb_user u ON u.id = s.user_id
        WHERE s.team_id = $1 AND s.puzzle_id = $2
            AND (NOT $5 OR s.saction > 0)
        ORDER BY s.ctime_at DESC, s.id DESC
        LIMIT $3 OFFSET $4;",
        team_id,
        puzzle_id,
        limit,
        page.saturating_mul(limit),
        only_ok,
    )
    .fetch_all(pool)
    .await?;

    let total = rows.first().and_then(|row| row.total).unwrap_or(0);
    let data = rows
        .into_iter()
        .map(|row| StaffPuzzleSubmission {
            id: row.id,
            user_id: row.user_id,
            user_name: row.user_name,
            user_answer: row.user_answer,
            norm_answer: row.norm_answer,
            saction: row.saction.into(),
            sresult: row.sresult,
            real_answer: row.real_answer,
            ignored: row.ignored,
            ctime_at: row.ctime_at,
        })
        .collect();

    Ok(Some(StaffPuzzleSubmissionPage { data, total }))
}

pub enum SubmitAnswerResult {
    Ok {
        result: JudgeResult,
        solved: bool,
        unlocks: Vec<i32>,
        cooldown_till: Option<OffsetDateTime>,
        update: SubmitStateBox,
        backend_events: Vec<crate::module::sync::PuzzleBackendEventSync>,
    },
    Locked,
    Duplicate,
    Invalid,
    NotFound,
}

async fn submit_requirements_conn(
    conn: &mut PgConnection,
    team_id: i32,
    game_id: i32,
    requirements: &[PuzzleSubmitRequirement],
) -> Result<bool, RbInternalError> {
    for requirement in requirements {
        let PuzzleSubmitRequirement::CurrencyMinimum {
            currency_id,
            minimum,
        } = requirement;
        let current = sqlx::query_scalar!(
            r#"SELECT CASE WHEN gf.state = 1 THEN
                    GREATEST(LEAST(tc.amount::NUMERIC, 0::NUMERIC), LEAST(
                        tc.amount::NUMERIC
                            + FLOOR(EXTRACT(EPOCH FROM (NOW() - tc.utime_at)) / 60)
                                * (c.growth + tc.growth)::NUMERIC,
                        c.max_amount::NUMERIC
                    ))::BIGINT
                ELSE tc.amount END AS "current_amount!"
            FROM rb_team_currency tc
            JOIN rb_currency c ON c.id = tc.currency_id
            JOIN rb_game_feature gf ON gf.game_id = c.game_id AND gf.feature_type = 4
            WHERE tc.team_id = $1 AND tc.currency_id = $2 AND c.game_id = $3
            FOR UPDATE OF tc"#,
            team_id,
            currency_id,
            game_id
        )
        .fetch_optional(&mut *conn)
        .await?;
        if current.is_none_or(|amount| amount < *minimum) {
            return Ok(false);
        }
    }
    Ok(true)
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

    sqlx::query_scalar!("SELECT id FROM rb_team WHERE id = $1 FOR UPDATE", team_id)
        .fetch_optional(&mut *tx)
        .await?;

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

    let puzzle_info = sqlx::query!(
        "SELECT id, game_id, round_id, title, submit_requirements, judge
        FROM rb_puzzle
        WHERE id = $1;",
        puzzle_id
    )
    .fetch_one(&mut *tx)
    .await?;
    let rules = game::judge::value_to_judge(puzzle_info.judge)?;
    if rules.is_empty() {
        return Ok(SubmitAnswerResult::Locked);
    }
    let Ok(requirements) = parse_submit_requirements(puzzle_info.submit_requirements.clone())
    else {
        return Ok(SubmitAnswerResult::Locked);
    };
    if !submit_requirements_conn(&mut tx, team_id, puzzle_info.game_id, &requirements).await? {
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

    let mut result = JudgeResult {
        action: RbJudgeAction::Fail,
        result: None,
        answer: None,
        ignored: false,
        triggers: Vec::new(),
    };
    let mut backend_events = Vec::new();
    for rule in rules.iter() {
        match rule.rtype.as_deref() {
            Some("exact") => {
                if let Some(expected) = &rule.text
                    && expected == &norm_answer
                {
                    result = JudgeResult {
                        action: rule.action.clone().into(),
                        result: rule.result.clone(),
                        answer: rule.answer.clone(),
                        ignored: false,
                        triggers: rule.triggers.clone(),
                    };
                    break;
                }
            }
            Some("custom") => {
                let backend_name = rule.function.clone().ok_or_else(|| {
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
                let team_info =
                    team_info.ok_or(RbInternalError::Other("team not found".to_string()))?;

                let execution = crate::module::puzzle_backend_js::execute_judge_conn(
                    app,
                    &mut tx,
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
                        norm_answer: norm_answer.clone(),
                        submission: submit_row.clone(),
                    },
                )
                .await?;
                backend_events.extend(execution.events);
                if let Some(output) = execution.value {
                    result = output.into();
                    break;
                }
            }
            Some("all") => {
                result = JudgeResult {
                    action: rule.action.clone().into(),
                    result: rule.result.clone(),
                    answer: rule.answer.clone(),
                    ignored: false,
                    triggers: rule.triggers.clone(),
                };
                break;
            }
            _ => {}
        }
    }

    if result
        .triggers
        .iter()
        .any(|key| !crate::game::judge::valid_trigger_key(key))
    {
        return Err(RbInternalError::Other(
            "judge returned an invalid trigger key".to_string(),
        ));
    }

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

    let mut content_changed = false;
    let mut trigger_inserted = false;

    if !result.ignored
        && !matches!(result.action, RbJudgeAction::Error)
        && !result.triggers.is_empty()
    {
        let inserted_triggers = sqlx::query!(
            "INSERT INTO rb_team_puzzle_trigger
                (team_id, puzzle_id, trigger_key, source_submission_id)
            SELECT $1, $2, trigger_key, $4
            FROM UNNEST($3::text[]) AS trigger_key
            ON CONFLICT DO NOTHING;",
            team_id,
            puzzle_id,
            &result.triggers,
            submit_id
        )
        .execute(&mut *tx)
        .await?;
        if inserted_triggers.rows_affected() > 0 {
            db::content::mark_team_dirty_conn(&mut tx, team_id).await?;
            content_changed = true;
            trigger_inserted = true;
        }
    }

    db::event_log::insert_conn(
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
                db::content::mark_team_dirty_conn(&mut tx, team_id).await?;
                content_changed = true;
                solved = true;
                do_unlock = true;
                db::event_log::insert_conn(
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
                db::ticket::close_puzzle_tickets_on_solve_conn(
                    &mut tx, team_id, puzzle_id, user.uid,
                )
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
                db::content::mark_team_dirty_conn(&mut tx, team_id).await?;
                content_changed = true;
                db::event_log::insert_conn(
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
                                db::event_log::insert_conn(
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
        }
        _ => {}
    }

    do_unlock = do_unlock || trigger_inserted;

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

    if result.action.side_effect() {
        db::board::LEADER_BOARD_CACHE
            .update_team(&app.db, team_id, true)
            .await?;
    }

    let unlocks = if do_unlock {
        unlock_new_puzzles(app, team_id).await?
    } else {
        vec![]
    };
    if do_unlock {
        refresh_team_hint_enablements(&app.db, team_id, None).await?;
    }
    content_changed = content_changed || !unlocks.is_empty();
    let has_custom_judge = rules
        .iter()
        .any(|rule| matches!(rule.rtype.as_deref(), Some("custom")));
    let update = SubmitStateBox(Box::new(SubmitStateUpdate {
        state: get_puzzle_team_state(&app.db, team_id, puzzle_id).await?,
        currency: if currency_updated
            || matches!(result.action, RbJudgeAction::StartGame)
            || has_custom_judge
        {
            db::team::get_currency_info(&app.db, team_id).await?
        } else {
            vec![]
        },
        currency_penalty,
        content_changed,
    }));

    Ok(SubmitAnswerResult::Ok {
        result,
        solved,
        unlocks,
        cooldown_till,
        update,
        backend_events,
    })
}

struct RbPuzzleStates {
    solved: HashSet<i32>,
    puzzle_slugs: HashMap<String, u32>,
    round_slugs: HashMap<String, u32>,
    round_puzzles: HashMap<u32, Vec<u32>>,
    triggers: HashSet<(i32, String)>,
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

    fn is_triggered(&self, id: expr::types::PuzzleId, key: &str) -> bool {
        self.triggers
            .contains(&(id.try_into().unwrap_or(i32::MAX), key.to_string()))
    }
}

async fn team_gate_state_conn(
    conn: &mut PgConnection,
    team_id: i32,
) -> Result<Option<RbPuzzleStates>, RbInternalError> {
    let team = sqlx::query!(
        "SELECT game_id, is_locked FROM rb_team WHERE id = $1",
        team_id
    )
    .fetch_optional(&mut *conn)
    .await?;
    let Some(team) = team else {
        return Ok(None);
    };

    let solved = sqlx::query_scalar!(
        "SELECT puzzle_id FROM rb_team_puzzle WHERE team_id = $1 AND state >= 1",
        team_id
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .collect();
    let triggers = sqlx::query!(
        "SELECT puzzle_id, trigger_key FROM rb_team_puzzle_trigger WHERE team_id = $1",
        team_id
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|row| (row.puzzle_id, row.trigger_key))
    .collect();
    let round_rows = sqlx::query!(
        "SELECT id, slug FROM rb_round WHERE game_id = $1",
        team.game_id
    )
    .fetch_all(&mut *conn)
    .await?;
    let puzzle_rows = sqlx::query!(
        "SELECT p.id, p.slug, p.round_id, r.puzzle AS round_puzzle_id
        FROM rb_puzzle p
        JOIN rb_round r ON r.id = p.round_id
        WHERE p.game_id = $1
        ORDER BY r.sort, r.id, (p.id IS DISTINCT FROM r.puzzle), p.sort, p.id",
        team.game_id
    )
    .fetch_all(&mut *conn)
    .await?;

    let round_slugs = round_rows
        .into_iter()
        .filter_map(|row| Some((row.slug?, row.id.try_into().ok()?)))
        .collect();
    let mut puzzle_slugs = HashMap::new();
    let mut round_puzzles: HashMap<u32, Vec<u32>> = HashMap::new();
    for row in puzzle_rows {
        let Ok(id) = row.id.try_into() else {
            continue;
        };
        if let Some(slug) = row.slug {
            puzzle_slugs.insert(slug, id);
        }
        if row.round_puzzle_id != Some(row.id)
            && let Ok(round_id) = row.round_id.try_into()
        {
            round_puzzles.entry(round_id).or_default().push(id);
        }
    }

    Ok(Some(RbPuzzleStates {
        solved,
        puzzle_slugs,
        round_slugs,
        round_puzzles,
        triggers,
        game_started: team.is_locked,
    }))
}

pub async fn refresh_team_hint_enablements(
    pool: &DbPool,
    team_id: i32,
    puzzle_id: Option<i32>,
) -> Result<(), RbInternalError> {
    let mut tx = pool.begin().await?;
    let pending = sqlx::query!(
        r#"SELECT h.id, h.enable_cond AS "enable_cond!"
        FROM rb_hint h
        JOIN rb_puzzle_effective_release release ON release.puzzle_id = h.puzzle_id
        JOIN rb_team_puzzle tp
            ON tp.puzzle_id = h.puzzle_id AND tp.team_id = $1 AND tp.state >= 0
        LEFT JOIN rb_team_hint_enable enabled
            ON enabled.team_id = $1 AND enabled.hint_id = h.id
        LEFT JOIN rb_team_hint purchased
            ON purchased.team_id = $1 AND purchased.hint_id = h.id AND purchased.unlocked
        WHERE h.enable_cond IS NOT NULL
            AND enabled.hint_id IS NULL
            AND purchased.hint_id IS NULL
            AND release.release_at <= NOW()
            AND ($2::INT IS NULL OR h.puzzle_id = $2)
        ORDER BY h.id"#,
        team_id,
        puzzle_id
    )
    .fetch_all(&mut *tx)
    .await?;

    if !pending.is_empty()
        && let Some(state) = team_gate_state_conn(&mut tx, team_id).await?
    {
        for hint in pending {
            let enabled = expr::compile_gate_expr(&hint.enable_cond)
                .ok()
                .is_some_and(|condition| expr::ast::eval_compiled(&state, &condition));
            if enabled {
                sqlx::query!(
                    "INSERT INTO rb_team_hint_enable (team_id, hint_id)
                    VALUES ($1, $2) ON CONFLICT DO NOTHING",
                    team_id,
                    hint.id
                )
                .execute(&mut *tx)
                .await?;
            }
        }
    }

    tx.commit().await?;
    Ok(())
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
    let triggers = sqlx::query!(
        "SELECT puzzle_id, trigger_key FROM rb_team_puzzle_trigger WHERE team_id = $1",
        team_id
    )
    .fetch_all(&app.db)
    .await?
    .into_iter()
    .map(|row| (row.puzzle_id, row.trigger_key))
    .collect();

    let round_rows = sqlx::query!(
        "SELECT id, slug
        FROM rb_round
        WHERE game_id = $1;",
        game_id
    )
    .fetch_all(&app.db)
    .await?;

    let puzzle_rows = sqlx::query!(
        "SELECT p.id, p.slug, p.round_id, p.unlock_cond, r.puzzle AS round_puzzle_id
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
    let mut conds = Vec::new();
    for row in puzzle_rows {
        let row_id = row.id;
        let puzzle_id = row_id.try_into().unwrap_or(0);
        if let Some(unlock_cond) = row.unlock_cond.as_deref() {
            match expr::compile_gate_expr(unlock_cond) {
                Ok(expr) => conds.push((row_id, expr)),
                Err(error) => {
                    log::warn!(
                        "Failed to parse unlock_cond for puzzle {}: {}",
                        row_id,
                        error
                    );
                }
            }
        }
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
        triggers,
        game_started: info[0].is_locked,
    };

    let mut unlocks: Vec<i32> = Vec::new();

    for cond in conds.iter() {
        if !state.is_solved(cond.0.try_into().unwrap_or(0))
            && expr::ast::eval_compiled(&state, &cond.1)
        {
            unlocks.push(cond.0);
        }
    }

    let mut inserted_unlocks = Vec::new();

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

        if !inserted.is_empty() {
            sqlx::query!(
                "UPDATE rb_team SET content_blocks_dirty = TRUE WHERE id = $1;",
                team_id
            )
            .execute(&app.db)
            .await?;
        }

        for puzzle in inserted {
            inserted_unlocks.push(puzzle.id);
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

    Ok(inserted_unlocks)
}

pub async fn admin_unlock_puzzle_for_eligible_teams(
    app: &AppState,
    puzzle_id: i32,
    game_id: i32,
    unlock_cond: Option<&str>,
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

    let compiled_unlock_cond = unlock_cond
        .map(expr::compile_gate_expr)
        .transpose()
        .map_err(RbInternalError::Other)?;

    let trigger_rows = sqlx::query!(
        "SELECT tpt.team_id, tpt.puzzle_id, tpt.trigger_key
        FROM rb_team_puzzle_trigger tpt
        JOIN rb_team t ON t.id = tpt.team_id
        WHERE t.game_id = $1",
        game_id
    )
    .fetch_all(&app.db)
    .await?;
    let mut team_triggers: HashMap<i32, HashSet<(i32, String)>> = HashMap::new();
    for row in trigger_rows {
        team_triggers
            .entry(row.team_id)
            .or_default()
            .insert((row.puzzle_id, row.trigger_key));
    }

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
            triggers: team_triggers.get(&team_id).cloned().unwrap_or_default(),
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

    if !inserted_team_ids.is_empty() {
        sqlx::query!(
            "UPDATE rb_team SET content_blocks_dirty = TRUE WHERE id = ANY($1);",
            &inserted_team_ids
        )
        .execute(&app.db)
        .await?;

        for team_id in &inserted_team_ids {
            refresh_team_hint_enablements(&app.db, *team_id, Some(puzzle_id)).await?;
        }
    }

    Ok(inserted_team_ids)
}

#[derive(Serialize)]
pub struct AdminClearPuzzleTeamStatesResult {
    pub team_count: usize,
    pub puzzle_states: usize,
    pub submissions: usize,
    pub hints: usize,
    pub tickets: usize,
    pub triggers: usize,
    pub team_ids: Vec<i32>,
}

pub async fn admin_clear_puzzle_team_states(
    pool: &DbPool,
    puzzle_id: i32,
) -> Result<AdminClearPuzzleTeamStatesResult, RbInternalError> {
    let mut tx = pool.begin().await?;

    let locked_team_ids = sqlx::query_scalar!(
        "SELECT t.id FROM rb_team t
        WHERE t.id IN (
            SELECT team_id FROM rb_team_puzzle WHERE puzzle_id = $1
            UNION
            SELECT team_id FROM rb_team_puzzle_trigger WHERE puzzle_id = $1
        )
        ORDER BY t.id FOR UPDATE;",
        puzzle_id
    )
    .fetch_all(&mut *tx)
    .await?;

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

    let mut hint_team_ids = sqlx::query_scalar!(
        "DELETE FROM rb_team_hint th
        USING rb_hint h
        WHERE th.hint_id = h.id AND h.puzzle_id = $1
        RETURNING th.team_id;",
        puzzle_id
    )
    .fetch_all(&mut *tx)
    .await?;

    hint_team_ids.extend(
        sqlx::query_scalar!(
            "DELETE FROM rb_team_hint_enable enabled
            USING rb_hint h
            WHERE enabled.hint_id = h.id AND h.puzzle_id = $1
            RETURNING enabled.team_id;",
            puzzle_id
        )
        .fetch_all(&mut *tx)
        .await?,
    );

    let ticket_team_ids = sqlx::query_scalar!(
        "DELETE FROM rb_ticket
        WHERE puzzle_id = $1
        RETURNING team_id;",
        puzzle_id
    )
    .fetch_all(&mut *tx)
    .await?;

    let trigger_team_ids = sqlx::query_scalar!(
        "DELETE FROM rb_team_puzzle_trigger
        WHERE puzzle_id = $1
        RETURNING team_id;",
        puzzle_id
    )
    .fetch_all(&mut *tx)
    .await?;

    if !locked_team_ids.is_empty() {
        sqlx::query!(
            "UPDATE rb_team SET content_blocks_dirty = TRUE WHERE id = ANY($1);",
            &locked_team_ids
        )
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    let mut team_ids = HashSet::new();
    team_ids.extend(puzzle_state_team_ids.iter().copied());
    team_ids.extend(submission_team_ids.iter().copied());
    team_ids.extend(hint_team_ids.iter().copied());
    team_ids.extend(ticket_team_ids.iter().copied());
    team_ids.extend(trigger_team_ids.iter().copied());

    let mut team_ids = team_ids.into_iter().collect::<Vec<_>>();
    team_ids.sort_unstable();

    Ok(AdminClearPuzzleTeamStatesResult {
        team_count: team_ids.len(),
        puzzle_states: puzzle_state_team_ids.len(),
        submissions: submission_team_ids.len(),
        hints: hint_team_ids.len(),
        tickets: ticket_team_ids.len(),
        triggers: trigger_team_ids.len(),
        team_ids,
    })
}

#[derive(FromRow, Serialize)]
pub struct RbHintShowData {
    pub id: i32,
    pub title: Option<String>,
    pub title_hidden: bool,
    pub cooldown: i32,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub available_at: OffsetDateTime,
    pub cost_id: Option<i32>,
    pub cost_amount: i64,
}

pub async fn get_hints_show_for_team(
    db_pool: &DbPool,
    team_id: i32,
    puzzle_id: i32,
) -> Result<Vec<RbHintShowData>, RbInternalError> {
    refresh_team_hint_enablements(db_pool, team_id, Some(puzzle_id)).await?;
    let result = sqlx::query_as!(
        RbHintShowData,
        "SELECT available.id,
            CASE
                WHEN available.title_hidden AND NOW() < available.available_at
                THEN NULL
                ELSE available.title
            END AS \"title?\",
            available.title_hidden, available.cooldown,
            available.available_at AS \"available_at!\",
            available.cost_id, available.cost_amount
        FROM (
            SELECT h.id, h.sort, h.title, h.title_hidden, h.cooldown,
                h.cost_id, h.cost_amount,
                (CASE
                    WHEN h.cooldown_after_enable
                        THEN COALESCE(enabled.enabled_at, purchased.utime_at)
                    ELSE GREATEST(tp.ctime_at, release.release_at)
                END) + (h.cooldown * INTERVAL '1 second') AS available_at
            FROM rb_hint h
            JOIN rb_puzzle p ON p.id = h.puzzle_id
            JOIN rb_puzzle_effective_release release ON release.puzzle_id = p.id
            JOIN rb_team_puzzle tp ON tp.puzzle_id = h.puzzle_id AND tp.team_id = $1
            LEFT JOIN rb_team_hint_enable enabled
                ON enabled.hint_id = h.id AND enabled.team_id = $1
            LEFT JOIN rb_team_hint purchased
                ON purchased.hint_id = h.id AND purchased.team_id = $1 AND purchased.unlocked
            WHERE p.id = $2 AND tp.state >= 0 AND release.release_at <= NOW()
                AND (h.enable_cond IS NULL OR enabled.hint_id IS NOT NULL
                    OR purchased.hint_id IS NOT NULL)
        ) available
        ORDER BY available.sort, available.id;",
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
        "SELECT MIN((CASE
                WHEN h.cooldown_after_enable THEN enabled.enabled_at
                ELSE GREATEST(tp.ctime_at, release.release_at)
            END) + (h.cooldown * INTERVAL '1 second')) AS next_unlock_at
        FROM rb_hint h
        JOIN rb_puzzle p ON p.id = h.puzzle_id
        JOIN rb_puzzle_effective_release release ON release.puzzle_id = p.id
        JOIN rb_team_puzzle tp ON tp.puzzle_id = h.puzzle_id AND tp.team_id = $1
        LEFT JOIN rb_team_hint_enable enabled
            ON enabled.hint_id = h.id AND enabled.team_id = $1
        WHERE h.puzzle_id = $2
            AND tp.state >= 0
            AND release.release_at <= NOW()
            AND (h.enable_cond IS NULL OR enabled.hint_id IS NOT NULL)
            AND h.title_hidden
            AND (CASE
                    WHEN h.cooldown_after_enable THEN enabled.enabled_at
                    ELSE GREATEST(tp.ctime_at, release.release_at)
                END) + (h.cooldown * INTERVAL '1 second') > NOW();",
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
    Ok {
        result: RbHintTeamStateShowData,
        backend_events: Vec<crate::module::sync::PuzzleBackendEventSync>,
    },
}

pub async fn purchase_hint(
    app: &AppState,
    user_id: i32,
    hint_id: i32,
) -> Result<PurchaseHintResult, RbInternalError> {
    let target = sqlx::query!(
        "SELECT tm.team_id, h.puzzle_id
        FROM rb_hint h
        JOIN rb_puzzle p ON p.id = h.puzzle_id
        JOIN rb_team_member tm ON tm.game_id = p.game_id
        WHERE tm.user_id = $1 AND h.id = $2",
        user_id,
        hint_id
    )
    .fetch_optional(&app.db)
    .await?;
    let Some(target) = target else {
        return Ok(PurchaseHintResult::Unavailable);
    };
    refresh_team_hint_enablements(&app.db, target.team_id, Some(target.puzzle_id)).await?;

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
        LEFT JOIN rb_team_hint_enable enabled
            ON enabled.hint_id = h.id AND enabled.team_id = tm.team_id
        WHERE tm.user_id = $1 AND h.id = $2 AND tp.state >= 0
            AND rp.release_at <= NOW()
            AND NOT COALESCE(th.unlocked, FALSE)
            AND (h.enable_cond IS NULL OR enabled.hint_id IS NOT NULL)
            AND (CASE
                    WHEN h.cooldown_after_enable THEN enabled.enabled_at
                    ELSE GREATEST(tp.ctime_at, rp.release_at)
                END) <= NOW() - (h.cooldown * INTERVAL '1 second');",
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

    let mut backend_events = Vec::new();
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
                "precision": currency.prec,
                "before": currency.before,
                "after": currency.after,
                "delta": currency.delta(),
            })
        });
        let execution = crate::module::puzzle_backend_js::execute_hint_purchase(
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
        backend_events = execution.events;
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

    db::event_log::insert_conn(
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
    Ok(PurchaseHintResult::Ok {
        result,
        backend_events,
    })
}

#[derive(Serialize)]
pub struct RbPuzzleAdminData {
    pub id: i32,
    pub game_id: i32,
    pub slug: Option<String>,
    pub sort: i32,
    pub title: String,
    pub ptype: i16,
    pub judge: serde_json::Value,
    pub penalty: serde_json::Value,
    pub submit_requirements: serde_json::Value,
    pub max_submit: Option<i32>,
    pub unlock_cond: Option<String>,
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
    #[serde(default = "default_submit_requirements")]
    pub submit_requirements: serde_json::Value,
    pub max_submit: Option<i32>,
    pub unlock_cond: Option<String>,
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
    pub judge: Option<serde_json::Value>,
    pub penalty: Option<serde_json::Value>,
    pub submit_requirements: Option<serde_json::Value>,
    #[serde(
        default,
        deserialize_with = "crate::serde_helpers::deserialize_nullable_i32_patch"
    )]
    pub max_submit: Option<Option<i32>>,
    #[serde(
        default,
        deserialize_with = "crate::serde_helpers::deserialize_nullable_string_patch"
    )]
    pub unlock_cond: Option<Option<String>>,
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
    serde_json::json!([])
}

fn default_penalty() -> serde_json::Value {
    serde_json::json!([])
}

fn default_submit_requirements() -> serde_json::Value {
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
            "SELECT p.id, r.game_id, p.slug, p.sort, p.title, p.ptype,
            p.judge, p.penalty, p.submit_requirements, p.max_submit, p.unlock_cond, p.release_phase_id,
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
            "SELECT p.id, r.game_id, p.slug, p.sort, p.title, p.ptype,
            p.judge, p.penalty, p.submit_requirements, p.max_submit, p.unlock_cond, p.release_phase_id,
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
        "SELECT p.id, r.game_id, p.slug, p.sort, p.title, p.ptype,
            p.judge, p.penalty, p.submit_requirements, p.max_submit, p.unlock_cond, p.release_phase_id,
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

async fn clear_immediate_release_events_conn(
    conn: &mut PgConnection,
    puzzle_ids: &[i32],
) -> Result<(), RbInternalError> {
    sqlx::query!(
        "DELETE FROM rb_release_event_puzzle rep
        USING rb_release_event re
        WHERE rep.event_id = re.id AND re.event_type = 1
            AND rep.puzzle_id = ANY($1);",
        puzzle_ids
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn create_immediate_release_event_conn(
    conn: &mut PgConnection,
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
    .fetch_one(&mut *conn)
    .await?;
    sqlx::query!(
        "INSERT INTO rb_release_event_puzzle (event_id, puzzle_id)
        SELECT $1, p.id FROM rb_puzzle p
        WHERE p.game_id = $2 AND p.id = ANY($3);",
        event_id,
        game_id,
        puzzle_ids
    )
    .execute(&mut *conn)
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
    .execute(&mut *conn)
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
            slug, sort, title, ptype, judge, penalty, submit_requirements,
            max_submit, unlock_cond, release_phase_id, immediate_release_at, round_id,
            ticket_enabled, ticket_cooldown
        )
        SELECT $2, $3, $4, $5, $6, $7, $14, $8, $9,
            CASE WHEN $11 THEN NULL ELSE $10::INT END,
            CASE WHEN $11 THEN NOW() ELSE NULL END,
            r.id, $12, $13
        FROM rb_round r
        WHERE r.id = $1
            AND NOT ($11 AND $10::INT IS NOT NULL)
            AND ($10::INT IS NULL OR EXISTS (
                SELECT 1 FROM rb_release_phase rp
                WHERE rp.id = $10::INT AND rp.game_id = r.game_id
                    AND rp.release_at > NOW()
                    AND NOT EXISTS (SELECT 1 FROM rb_release_event re WHERE re.phase_id = rp.id)
            ))
        RETURNING id, game_id,
            slug, sort, title, ptype, judge, penalty, submit_requirements,
            max_submit, unlock_cond, release_phase_id, immediate_release_at, round_id,
            ticket_enabled, ticket_cooldown, ctime_at;",
        data.round_id,
        data.slug,
        data.sort,
        data.title,
        data.ptype,
        data.judge,
        data.penalty,
        data.max_submit,
        data.unlock_cond,
        data.release_phase_id,
        data.release_immediately,
        data.ticket_enabled,
        data.ticket_cooldown,
        data.submit_requirements
    )
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(puzzle) = &result {
        sqlx::query!(
            "INSERT INTO rb_content_block (puzzle_id, sort, name, content, content_type)
            VALUES ($1, 0, 'Default', $2, $3);",
            puzzle.id,
            data.content,
            data.content_type
        )
        .execute(&mut *tx)
        .await?;
    }
    if let Some(puzzle) = &result
        && let Some(released_at) = puzzle.immediate_release_at
    {
        create_immediate_release_event_conn(&mut tx, puzzle.game_id, &[puzzle.id], released_at)
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
        clear_immediate_release_events_conn(&mut tx, &[puzzle_id]).await?;
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
            judge = COALESCE($7, p.judge),
            penalty = COALESCE($8, p.penalty),
            submit_requirements = COALESCE($19, p.submit_requirements),
            max_submit = CASE WHEN $9 THEN $10 ELSE p.max_submit END,
            unlock_cond = CASE WHEN $11 THEN $12 ELSE p.unlock_cond END,
            release_phase_id = CASE
                WHEN $15 THEN NULL
                WHEN $13 THEN $14
                ELSE p.release_phase_id
            END,
            immediate_release_at = CASE
                WHEN $15 THEN NOW()
                WHEN $13 THEN NULL
                ELSE p.immediate_release_at
            END,
            round_id = COALESCE((
                SELECT r.id FROM rb_round r WHERE r.id = $16::INT
            ), p.round_id),
            ticket_enabled = COALESCE($17, p.ticket_enabled),
            ticket_cooldown = COALESCE($18, p.ticket_cooldown)
        WHERE p.id = $1
            AND NOT ($15 AND $13)
            AND (NOT $13 OR $14::INT IS NULL OR EXISTS (
                SELECT 1 FROM rb_release_phase target_phase
                WHERE target_phase.id = $14::INT AND target_phase.game_id = p.game_id
                    AND target_phase.release_at > NOW()
                    AND NOT EXISTS (SELECT 1 FROM rb_release_event target_event WHERE target_event.phase_id = target_phase.id)
            ))
            AND ($16::INT IS NULL OR EXISTS (
                SELECT 1 FROM rb_round target_round WHERE target_round.id = $16::INT
            ))
            AND ($16::INT IS NULL OR NOT EXISTS (
                SELECT 1 FROM rb_round owner_round
                WHERE owner_round.puzzle = p.id AND owner_round.id IS DISTINCT FROM $16::INT
            ))
        RETURNING p.id, p.game_id,
            p.slug, p.sort, p.title, p.ptype,
            p.judge, p.penalty, p.submit_requirements, p.max_submit, p.unlock_cond, p.release_phase_id,
            p.immediate_release_at, p.round_id,
            p.ticket_enabled, p.ticket_cooldown, p.ctime_at;",
        puzzle_id,
        slug_is_set,
        slug,
        data.sort,
        data.title,
        data.ptype,
        data.judge,
        data.penalty,
        max_submit_is_set,
        max_submit,
        data.unlock_cond.is_some(),
        data.unlock_cond.clone().flatten(),
        release_phase_is_set,
        release_phase_id,
        release_immediately,
        data.round_id,
        data.ticket_enabled,
        data.ticket_cooldown,
        data.submit_requirements
    )
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(puzzle) = &result
        && release_immediately
        && let Some(released_at) = puzzle.immediate_release_at
    {
        create_immediate_release_event_conn(&mut tx, puzzle.game_id, &[puzzle.id], released_at)
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
    clear_immediate_release_events_conn(&mut tx, puzzle_ids).await?;
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
        RETURNING p.id, p.game_id, p.slug, p.sort, p.title, p.ptype,
            p.judge, p.penalty, p.submit_requirements, p.max_submit, p.unlock_cond,
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
        create_immediate_release_event_conn(&mut tx, game_id, puzzle_ids, released_at).await?;
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
    pub enable_cond: Option<String>,
    pub cooldown_after_enable: bool,
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
    pub enable_cond: Option<String>,
    #[serde(default)]
    pub cooldown_after_enable: bool,
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
        deserialize_with = "crate::serde_helpers::deserialize_nullable_string_patch"
    )]
    pub enable_cond: Option<Option<String>>,
    pub cooldown_after_enable: Option<bool>,
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
            "SELECT id, sort, title, title_hidden, content, content_type, cooldown,
                enable_cond, cooldown_after_enable, cost_id,
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
            "SELECT id, sort, title, title_hidden, content, content_type, cooldown,
                enable_cond, cooldown_after_enable, cost_id,
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
        "SELECT id, sort, title, title_hidden, content, content_type, cooldown,
            enable_cond, cooldown_after_enable, cost_id,
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
            sort, title, title_hidden, content, content_type, cooldown,
            enable_cond, cooldown_after_enable, cost_id, cost_amount,
            backend_function, puzzle_id
        )
        SELECT $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, p.id
        FROM rb_puzzle p
        WHERE p.id = $1
        RETURNING id, sort, title, title_hidden, content, content_type, cooldown,
            enable_cond, cooldown_after_enable, cost_id,
            cost_amount, backend_function, puzzle_id, ctime_at;",
        data.puzzle_id,
        data.sort,
        data.title,
        data.title_hidden,
        data.content,
        data.content_type,
        data.cooldown,
        data.enable_cond,
        data.cooldown_after_enable,
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
    let enable_cond_is_set = data.enable_cond.is_some();
    let enable_cond = data.enable_cond.clone().flatten();

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
            enable_cond = CASE WHEN $13 THEN $14 ELSE h.enable_cond END,
            cooldown_after_enable = CASE
                WHEN $13 AND $14::TEXT IS NULL THEN FALSE
                ELSE COALESCE($15, h.cooldown_after_enable)
            END,
            puzzle_id = COALESCE((
                SELECT p.id FROM rb_puzzle p WHERE p.id = $16::INT
            ), h.puzzle_id)
        WHERE h.id = $1
            AND ($16::INT IS NULL OR EXISTS (
                SELECT 1 FROM rb_puzzle p WHERE p.id = $16::INT
            ))
        RETURNING id, sort, title, title_hidden, content, content_type, cooldown,
            enable_cond, cooldown_after_enable, cost_id,
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
        enable_cond_is_set,
        enable_cond,
        data.cooldown_after_enable,
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
