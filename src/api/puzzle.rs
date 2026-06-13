use actix_session::SessionExt;
use actix_web::{
    HttpMessage, HttpResponse, Result,
    body::MessageBody,
    dev::{ServiceRequest, ServiceResponse},
    http::header::ContentType,
    middleware::{self, Next},
    web,
};
use num_enum::IntoPrimitive;
use serde::{Deserialize, Serialize};
use serde_repr::Serialize_repr;
use time::OffsetDateTime;

use crate::{
    AppState,
    api::{error_handler, puzzle_backend, ticket},
    db::{self},
    error::{RbError, RbInternalError},
    extractor::auth::AuthUser,
    game::judge::JudgeResult,
    module::sync::{PuzzleHintUnlockedSync, PuzzleSubmittedSync, PuzzleUnlockInfo},
};

#[derive(Deserialize)]
pub struct PuzzlePathInfo {
    pub puzzle_id: i32,
}

#[derive(Deserialize)]
struct HintPathInfo {
    hint_id: i32,
}

async fn get_puzzle(
    path: web::Path<PuzzlePathInfo>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let result = db::puzzle::get_puzzle_show_str_for_team(
        &app.db,
        &app.kv,
        user.req_team_id()?.ok_or(RbError::forbid())?,
        path.puzzle_id,
    )
    .await?;
    if result.is_none() {
        RbError::not_found().err()?
    }

    Ok(HttpResponse::Ok()
        .content_type(ContentType::json())
        .body(result.unwrap()))
}

#[derive(Deserialize)]
struct PuzzleJudgeRequest {
    answer: String,
    sid: Option<String>,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum PuzzleJudgeResult {
    Locked = -3,
    Duplicate = -2,
    Invalid = -1,
}

#[derive(Serialize)]
struct JudgePuzzleResponse {
    result: JudgeResult,
    #[serde(with = "crate::serde_helpers::serialize_option_offset_datetime")]
    cooldown_till: Option<OffsetDateTime>,
    solved: bool,
    unlocks: Vec<PuzzleUnlockInfo>,
    state: Option<db::puzzle::RbPuzzleTeamStateShowData>,
    currency: Vec<db::team::RbCurrencyShowData>,
    currency_penalty: Vec<db::puzzle::CurrencyPenaltyShowData>,
}

async fn judge_puzzle(
    path: web::Path<PuzzlePathInfo>,
    req: web::Json<PuzzleJudgeRequest>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let team_id = user.req_team_id()?.ok_or(RbError::forbid())?;
    let submit_result = db::puzzle::submit_answer(&app, &user, path.puzzle_id, &req.answer).await?;

    match submit_result {
        db::puzzle::SubmitAnswerResult::NotFound => RbError::not_found().http_err(),
        db::puzzle::SubmitAnswerResult::Duplicate => {
            RbError::conflict(PuzzleJudgeResult::Duplicate.into()).http_err()
        }
        db::puzzle::SubmitAnswerResult::Invalid => {
            RbError::bad_req(PuzzleJudgeResult::Invalid.into()).http_err()
        }
        db::puzzle::SubmitAnswerResult::Locked => {
            RbError::conflict(PuzzleJudgeResult::Locked.into()).http_err()
        }
        db::puzzle::SubmitAnswerResult::Ok {
            result,
            solved,
            unlocks,
            cooldown_till,
            update,
        } => {
            let update = *update.0;
            let unlock_rows = sqlx::query_as!(
                PuzzleUnlockInfo,
                "SELECT p.id, p.slug, p.title, p.round_id, r.slug AS round_slug
                FROM rb_puzzle p
                JOIN rb_round r ON r.id = p.round_id
                WHERE p.id = ANY($1)
                ORDER BY r.sort, r.id, (p.id IS DISTINCT FROM r.puzzle), p.sort, p.id",
                &unlocks
            )
            .fetch_all(&app.db)
            .await
            .map_err(RbInternalError::from)?;

            let answer = req.answer.clone();
            let sid = req.sid.clone();
            let sync_unlocks = unlock_rows.clone();
            let action = result.action;
            let sync_state = update.state.clone();
            let sync_currency = update.currency.clone();
            let sync_currency_penalty = update.currency_penalty.clone();

            tokio::spawn(async move {
                let _ = app
                    .sync_hub
                    .notify_puzzle_submitted(
                        &app.db,
                        PuzzleSubmittedSync {
                            team_id,
                            user_id: user.uid,
                            puzzle_id: path.puzzle_id,
                            answer,
                            action,
                            cooldown_till,
                            solved,
                            unlocks: sync_unlocks,
                            state: sync_state,
                            currency: sync_currency,
                            currency_penalty: sync_currency_penalty,
                            sid,
                        },
                    )
                    .await;
            });

            Ok(HttpResponse::Ok().json(JudgePuzzleResponse {
                result,
                cooldown_till,
                solved,
                unlocks: unlock_rows,
                state: update.state,
                currency: update.currency,
                currency_penalty: update.currency_penalty,
            }))
        }
    }
}

#[derive(Deserialize)]
pub struct SubmissionQuery {
    page: Option<i64>,
    only_ok: Option<bool>,
}

async fn get_puzzle_hints(
    path: web::Path<PuzzlePathInfo>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let result = db::puzzle::get_hints_show_str_for_team(
        &app.db,
        &app.kv,
        user.req_team_id()?.ok_or(RbError::forbid())?,
        path.puzzle_id,
    )
    .await?;

    Ok(HttpResponse::Ok()
        .content_type(ContentType::json())
        .body(result))
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
pub enum PurchaseHintResult {
    Insufficient = -2,
    Unavailable = -1,
    Ok = 0,
}

#[derive(Deserialize)]
struct SidRequest {
    sid: Option<String>,
}

async fn purchase_hint(
    path: web::Path<HintPathInfo>,
    req: Option<web::Json<SidRequest>>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let team_id = user.req_team_id()?.ok_or(RbError::forbid())?;
    let purchase_result = db::puzzle::purchase_hint(&app, user.uid, path.hint_id).await?;

    match purchase_result {
        db::puzzle::PurchaseHintResult::Unavailable => {
            RbError::conflict(PurchaseHintResult::Unavailable.into()).http_err()
        }
        db::puzzle::PurchaseHintResult::Insufficient => {
            RbError::conflict(PurchaseHintResult::Insufficient.into()).http_err()
        }
        db::puzzle::PurchaseHintResult::Ok(result) => {
            let sid = req.and_then(|req| req.sid.clone());

            tokio::spawn(async move {
                let _ = app
                    .sync_hub
                    .notify_puzzle_hint_unlocked(
                        &app.db,
                        PuzzleHintUnlockedSync {
                            team_id,
                            user_id: user.uid,
                            hint_id: path.hint_id,
                            sid,
                        },
                    )
                    .await;
            });

            Ok(HttpResponse::Ok().json(result))
        }
    }
}

async fn get_puzzle_submissions(
    path: web::Path<PuzzlePathInfo>,
    req: web::Query<SubmissionQuery>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let result = db::puzzle::get_team_submissions(
        &app.db,
        user.req_team_id()?.ok_or(RbError::forbid())?,
        path.puzzle_id,
        req.page.unwrap_or(0),
        req.only_ok.unwrap_or(false),
    )
    .await?;

    Ok(HttpResponse::Ok().json(result))
}

async fn check_puzzle_middleware(
    req: ServiceRequest,
    next: Next<impl MessageBody>,
) -> Result<ServiceResponse<impl MessageBody>, actix_web::Error> {
    let puzzle_id: i32 = req
        .match_info()
        .get("puzzle_id")
        .and_then(|s| s.parse().ok())
        .ok_or_else(RbError::not_found)?;

    let user_id: i32 = req
        .get_session()
        .get::<i32>("user_id")
        .ok()
        .flatten()
        .ok_or_else(RbError::not_found)?;

    let app = req.app_data::<web::Data<AppState>>().unwrap();

    match db::puzzle::get_puzzle_user_info(&app.db, user_id, puzzle_id).await? {
        Some(info) => {
            req.extensions_mut().insert(info);
        }
        None => {
            RbError::not_found().err()?;
        }
    };

    next.call(req).await
}

async fn check_hint_middleware(
    req: ServiceRequest,
    next: Next<impl MessageBody>,
) -> Result<ServiceResponse<impl MessageBody>, actix_web::Error> {
    let hint_id: i32 = req
        .match_info()
        .get("hint_id")
        .and_then(|s| s.parse().ok())
        .ok_or_else(RbError::not_found)?;

    let user_id: i32 = req
        .get_session()
        .get::<i32>("user_id")
        .ok()
        .flatten()
        .ok_or_else(RbError::not_found)?;

    let app = req.app_data::<web::Data<AppState>>().unwrap();

    match db::puzzle::get_hint_user_info(&app.db, &app.kv, user_id, hint_id).await? {
        Some(info) => {
            req.extensions_mut().insert(info);
        }
        None => {
            RbError::not_found().err()?;
        }
    };

    next.call(req).await
}

// /puzzles/...
pub fn puzzles_config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/{puzzle_id}")
            .wrap(middleware::from_fn(check_puzzle_middleware))
            .route("", web::get().to(get_puzzle))
            .route("/submit", web::post().to(judge_puzzle))
            .route("/hints", web::get().to(get_puzzle_hints))
            .route("/submissions", web::get().to(get_puzzle_submissions))
            .configure(puzzle_backend::config)
            .service(
                web::scope("/tickets")
                    .configure(ticket::puzzles_config)
                    .default_service(web::route().to(error_handler)),
            )
            .default_service(web::route().to(error_handler)),
    );
}

// /hints/...
pub fn hints_config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/{hint_id}")
            .wrap(middleware::from_fn(check_hint_middleware))
            .route("/purchase", web::post().to(purchase_hint))
            .default_service(web::route().to(error_handler)),
    );
}
