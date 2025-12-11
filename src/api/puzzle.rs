use actix_session::SessionExt;
use actix_web::{
    HttpResponse, Result,
    body::MessageBody,
    dev::{ServiceRequest, ServiceResponse},
    middleware::{self, Next},
    web,
};
use num_enum::IntoPrimitive;
use serde::{Deserialize, Serialize};
use serde_repr::Serialize_repr;

use crate::{
    DbPool, KvPool,
    api::error_handler,
    db::{
        self,
        puzzle::{PuzzleSource, SubmitAnswerData},
    },
    error::RbError,
    extractor::auth::AuthUser,
    game::puzzle::JudgeResult,
};

#[derive(Deserialize)]
struct GamePathInfo {
    game_id: i32,
}

#[derive(Deserialize)]
struct PuzzlePathInfo {
    puzzle_id: i32,
}

async fn get_intro(
    info: web::Path<GamePathInfo>,
    db_pool: web::Data<DbPool>,
) -> Result<HttpResponse> {
    let source = PuzzleSource::new_intro(info.game_id);
    let result = db::puzzle::get_puzzle_show(&db_pool, &source).await?;
    if result.is_none() {
        RbError::not_found().err()?
    }

    Ok(HttpResponse::Ok().json(result))
}

async fn get_puzzle(
    info: web::Path<PuzzlePathInfo>,
    db_pool: web::Data<DbPool>,
) -> Result<HttpResponse> {
    let source = PuzzleSource::new(info.puzzle_id);
    let result = db::puzzle::get_puzzle_show(&db_pool, &source).await?;
    if result.is_none() {
        RbError::not_found().err()?
    }

    Ok(HttpResponse::Ok().json(result))
}

#[derive(Deserialize)]
struct PuzzleJudgeRequest {
    answer: String,
}

#[derive(Serialize)]
struct PuzzleJudgeResponse {
    code: PuzzleJudgeResult,
    result: JudgeResult,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum PuzzleJudgeResult {
    Duplicate = -2,
    Invalid = -1,
    Ok = 0,
}

async fn judge_intro(
    req: web::Json<PuzzleJudgeRequest>,
    info: web::Path<GamePathInfo>,
    user: AuthUser,
    db_pool: web::Data<DbPool>,
) -> Result<HttpResponse> {
    let data = SubmitAnswerData::new_intro(user.uid, info.game_id, &req.answer);
    let submit_result = db::puzzle::submit_answer(&db_pool, &data).await?;

    match submit_result {
        db::puzzle::SubmitAnswerResult::NotFound => RbError::not_found().http_err(),
        db::puzzle::SubmitAnswerResult::Duplicate => {
            RbError::conflict(PuzzleJudgeResult::Duplicate.into()).http_err()
        }
        db::puzzle::SubmitAnswerResult::Invalid => {
            RbError::bad_req(PuzzleJudgeResult::Invalid.into()).http_err()
        }
        db::puzzle::SubmitAnswerResult::Ok(result) => {
            Ok(HttpResponse::Ok().json(PuzzleJudgeResponse {
                code: PuzzleJudgeResult::Ok,
                result,
            }))
        }
    }
}

async fn judge_puzzle(
    req: web::Json<PuzzleJudgeRequest>,
    info: web::Path<PuzzlePathInfo>,
    user: AuthUser,
    db_pool: web::Data<DbPool>,
) -> Result<HttpResponse> {
    let data = SubmitAnswerData::new(user.uid, info.puzzle_id, &req.answer);
    let submit_result = db::puzzle::submit_answer(&db_pool, &data).await?;

    match submit_result {
        db::puzzle::SubmitAnswerResult::NotFound => RbError::not_found().http_err(),
        db::puzzle::SubmitAnswerResult::Duplicate => {
            RbError::conflict(PuzzleJudgeResult::Duplicate.into()).http_err()
        }
        db::puzzle::SubmitAnswerResult::Invalid => {
            RbError::bad_req(PuzzleJudgeResult::Invalid.into()).http_err()
        }
        db::puzzle::SubmitAnswerResult::Ok(result) => {
            Ok(HttpResponse::Ok().json(PuzzleJudgeResponse {
                code: PuzzleJudgeResult::Ok,
                result,
            }))
        }
    }
}

#[derive(Deserialize)]
pub struct SubmissionQuery {
    page: Option<i64>,
}

async fn get_intro_submissions(
    req: web::Query<SubmissionQuery>,
    info: web::Path<GamePathInfo>,
    user: AuthUser,
    db_pool: web::Data<DbPool>,
) -> Result<HttpResponse> {
    let source = PuzzleSource::new_intro(info.game_id);
    let result = db::puzzle::get_team_submissions_by_user(
        &db_pool,
        user.uid,
        &source,
        req.page.unwrap_or(0),
    )
    .await?;

    Ok(HttpResponse::Ok().json(result))
}

async fn get_puzzle_submissions(
    req: web::Query<SubmissionQuery>,
    info: web::Path<PuzzlePathInfo>,
    user: AuthUser,
    db_pool: web::Data<DbPool>,
) -> Result<HttpResponse> {
    let source = PuzzleSource::new(info.puzzle_id);
    let result = db::puzzle::get_team_submissions_by_user(
        &db_pool,
        user.uid,
        &source,
        req.page.unwrap_or(0),
    )
    .await?;

    Ok(HttpResponse::Ok().json(result))
}

async fn check_intro_middleware(
    req: ServiceRequest,
    next: Next<impl MessageBody>,
) -> Result<ServiceResponse<impl MessageBody>, actix_web::Error> {
    let game_id: i32 = req
        .match_info()
        .get("game_id")
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

    let source = PuzzleSource::new_intro(game_id);
    if !db::puzzle::check_user_access(db_pool, kv_pool, user_id, &source).await? {
        RbError::not_found().err()?;
    }

    next.call(req).await
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

    let source = PuzzleSource::new(puzzle_id);
    if !db::puzzle::check_user_access(db_pool, kv_pool, user_id, &source).await? {
        RbError::not_found().err()?;
    }

    next.call(req).await
}

// /games/{game_id}/puzzles/...
pub fn games_config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/intro")
            .wrap(middleware::from_fn(check_intro_middleware))
            .route("", web::get().to(get_intro))
            .route("/submit", web::post().to(judge_intro))
            .route("/submissions", web::get().to(get_intro_submissions))
            .default_service(web::route().to(error_handler)),
    );
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
