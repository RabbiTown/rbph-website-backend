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
use serde_json::json;
use serde_repr::Serialize_repr;
use time::OffsetDateTime;

use crate::{
    AppState,
    api::error_handler,
    db::{self},
    error::RbError,
    extractor::auth::AuthUser,
    game::judge::JudgeResult,
    module::sync::SyncMessageType,
    serde_helpers::serialize_option_offset_datetime,
};

#[derive(Deserialize)]
struct PuzzlePathInfo {
    puzzle_id: i32,
}

#[derive(Deserialize)]
struct HintPathInfo {
    hint_id: i32,
}

async fn get_puzzle(
    info: web::Path<PuzzlePathInfo>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let result = db::puzzle::get_puzzle_show_str_for_team(
        &app.db,
        &app.kv,
        user.get_team_id().ok_or(RbError::forbid())?,
        info.puzzle_id,
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
}

async fn judge_puzzle(
    req: web::Json<PuzzleJudgeRequest>,
    info: web::Path<PuzzlePathInfo>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let submit_result = db::puzzle::submit_answer(&app, &user, info.puzzle_id, &req.answer).await?;

    match submit_result {
        db::puzzle::SubmitAnswerResult::NotFound => RbError::not_found().http_err(),
        db::puzzle::SubmitAnswerResult::Duplicate => {
            RbError::conflict(PuzzleJudgeResult::Duplicate.into()).http_err()
        }
        db::puzzle::SubmitAnswerResult::Invalid => {
            RbError::bad_req(PuzzleJudgeResult::Invalid.into()).http_err()
        }
        db::puzzle::SubmitAnswerResult::Locked => {
            RbError::bad_req(PuzzleJudgeResult::Locked.into()).http_err()
        }
        db::puzzle::SubmitAnswerResult::Ok {
            result,
            solved,
            unlocks,
            cooldown_till,
        } => {
            tokio::spawn(async move {
                let row = sqlx::query!(
                    "SELECT
                        (SELECT nickname FROM rb_user WHERE id = $1) AS u_n,
                        (SELECT title FROM rb_puzzle WHERE id = $2) AS p_t;",
                    user.uid,
                    info.puzzle_id
                )
                .fetch_one(&app.db)
                .await;

                let unlock_rows = sqlx::query!(
                    "SELECT id, title, round_id FROM rb_puzzle WHERE id = ANY($1)",
                    &unlocks
                )
                .fetch_one(&app.db)
                .await;

                if let Ok(row) = row {
                    let mut sync = json!({
                        "user": {
                            "id": user.uid,
                            "name": row.u_n,
                        },
                        "puzzle": {
                            "id": info.puzzle_id,
                            "title": row.p_t,
                        },
                        "answer": req.answer,
                        "action": result.action,
                    });
                    if cooldown_till.is_some()
                        && let Ok(x) = serialize_option_offset_datetime::serialize(
                            &cooldown_till,
                            serde_json::value::Serializer,
                        )
                    {
                        sync["cooldown_till"] = x;
                    }
                    if solved {
                        sync["solved"] = json!(true);
                        sync["unlocks"] = json!(
                            unlock_rows
                                .into_iter()
                                .map(|r| {
                                    json!({
                                        "id": r.id,
                                        "title": r.title,
                                        "round_id": r.round_id,
                                    })
                                })
                                .collect::<Vec<_>>()
                        );
                    }
                    let _ = app
                        .sync_hub
                        .do_push_team(
                            &app.db,
                            user.get_team_id().unwrap(),
                            SyncMessageType::PuzzleSubmitted,
                            sync,
                        )
                        .await;
                }
            });

            Ok(HttpResponse::Ok().json(JudgePuzzleResponse {
                result,
                cooldown_till,
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
    info: web::Path<PuzzlePathInfo>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let result = db::puzzle::get_hints_show_str_for_team(
        &app.db,
        &app.kv,
        user.get_team_id().ok_or(RbError::forbid())?,
        info.puzzle_id,
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

async fn purchase_hint(
    info: web::Path<HintPathInfo>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let purchase_result = db::puzzle::purchase_hint(&app, user.uid, info.hint_id).await?;

    match purchase_result {
        db::puzzle::PurchaseHintResult::Unavailable => {
            RbError::conflict(PurchaseHintResult::Unavailable.into()).http_err()
        }
        db::puzzle::PurchaseHintResult::Insufficient => {
            RbError::conflict(PurchaseHintResult::Insufficient.into()).http_err()
        }
        db::puzzle::PurchaseHintResult::Ok(result) => {
            tokio::spawn(async move {
                let row = sqlx::query!(
                    "SELECT (SELECT nickname FROM rb_user WHERE id = $1) AS u_n,
                            h.title AS h_t, h.cost_id AS h_ci, h.cost_amount AS h_ca,
                            p.title AS p_t, p.id AS p_i
                    FROM rb_hint h
                    JOIN rb_puzzle p ON p.id = h.puzzle_id
                    WHERE h.id = $2",
                    user.uid,
                    info.hint_id
                )
                .fetch_one(&app.db)
                .await;

                if let Ok(row) = row {
                    let sync = json!({
                        "user": {
                            "id": user.uid,
                            "name": row.u_n,
                        },
                        "puzzle": {
                            "id": row.p_i,
                            "title": row.p_t,
                        },
                        "hint": {
                            "id": info.hint_id,
                            "title": row.h_t,
                            "cost_id": row.h_ci,
                            "cost_amount": row.h_ca
                        }
                    });
                    let _ = app
                        .sync_hub
                        .do_push_team(
                            &app.db,
                            user.get_team_id().unwrap(),
                            SyncMessageType::PuzzleHintUnlocked,
                            sync,
                        )
                        .await;
                }
            });

            Ok(HttpResponse::Ok().json(result))
        }
    }
}

async fn get_puzzle_submissions(
    req: web::Query<SubmissionQuery>,
    info: web::Path<PuzzlePathInfo>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let result = db::puzzle::get_team_submissions(
        &app.db,
        user.get_team_id().ok_or(RbError::forbid())?,
        info.puzzle_id,
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
