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

use crate::{
    DbPool, KvPool,
    api::error_handler,
    db::{self},
    error::RbError,
    extractor::auth::AuthUser,
    game::puzzle::JudgeResult,
};

#[derive(Deserialize)]
struct PuzzlePathInfo {
    puzzle_id: i32,
}

async fn get_puzzle(
    info: web::Path<PuzzlePathInfo>,
    user: AuthUser,
    db_pool: web::Data<DbPool>,
    kv_pool: web::Data<KvPool>,
) -> Result<HttpResponse> {
    let result = db::puzzle::get_puzzle_show_str_for_team(
        &db_pool,
        &kv_pool,
        user.game.unwrap().team_id,
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
    Duplicate = -2,
    Invalid = -1,
}

async fn judge_puzzle(
    req: web::Json<PuzzleJudgeRequest>,
    info: web::Path<PuzzlePathInfo>,
    user: AuthUser,
    db_pool: web::Data<DbPool>,
    kv_pool: web::Data<KvPool>,
) -> Result<HttpResponse> {
    let submit_result = db::puzzle::submit_answer(
        &db_pool,
        &kv_pool,
        user.uid,
        user.game.unwrap().team_id,
        info.puzzle_id,
        &req.answer,
    )
    .await?;

    match submit_result {
        db::puzzle::SubmitAnswerResult::NotFound => RbError::not_found().http_err(),
        db::puzzle::SubmitAnswerResult::Duplicate => {
            RbError::conflict(PuzzleJudgeResult::Duplicate.into()).http_err()
        }
        db::puzzle::SubmitAnswerResult::Invalid => {
            RbError::bad_req(PuzzleJudgeResult::Invalid.into()).http_err()
        }
        db::puzzle::SubmitAnswerResult::Ok(result) => Ok(HttpResponse::Ok().json(result)),
    }
}

#[derive(Deserialize)]
pub struct SubmissionQuery {
    page: Option<i64>,
    only_ok: Option<bool>,
}

async fn get_puzzle_submissions(
    req: web::Query<SubmissionQuery>,
    info: web::Path<PuzzlePathInfo>,
    user: AuthUser,
    db_pool: web::Data<DbPool>,
) -> Result<HttpResponse> {
    let result = db::puzzle::get_team_submissions(
        &db_pool,
        user.game.unwrap().team_id,
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
        .ok_or_else(|| RbError::not_found())?;

    let user_id: i32 = req
        .get_session()
        .get::<i32>("user_id")
        .ok()
        .flatten()
        .ok_or_else(|| RbError::not_found())?;

    let db_pool = req.app_data::<web::Data<DbPool>>().unwrap();
    let kv_pool = req.app_data::<web::Data<KvPool>>().unwrap();

    match db::puzzle::get_puzzle_user_info(db_pool, kv_pool, user_id, puzzle_id).await? {
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
            .route("/submissions", web::get().to(get_puzzle_submissions))
            .default_service(web::route().to(error_handler)),
    );
}
