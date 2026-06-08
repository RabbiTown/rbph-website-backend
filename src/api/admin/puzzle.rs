use actix_web::{HttpResponse, Result, web};
use deadpool_redis::redis::AsyncCommands;
use num_enum::IntoPrimitive;
use serde::{Deserialize, Serialize};
use serde_repr::Serialize_repr;

use crate::{
    AppState,
    db::{
        self,
        puzzle::{RbPuzzleAdminData, RbPuzzleCreateData, RbPuzzleUpdateData},
    },
    error::{RbError, RbInternalError},
    expr,
    model::game::{RbContentType, RbPuzzleType},
};

fn is_constraint_error(err: &RbInternalError) -> bool {
    matches!(
        err,
        RbInternalError::Sql(sqlx::Error::Database(db_err))
            if db_err.code().is_some_and(|code| code == "23505" || code == "23514")
    )
}

#[derive(Deserialize)]
struct PuzzlePathInfo {
    puzzle_id: i32,
}

#[derive(Deserialize)]
struct PuzzleListQuery {
    game_id: Option<i32>,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum PuzzleAdminResult {
    Invalid = -2,
    NotFound = -1,
    Ok = 0,
}

#[derive(Serialize)]
struct PuzzleAdminResponse {
    code: PuzzleAdminResult,
    puzzle: RbPuzzleAdminData,
}

#[derive(Serialize)]
struct PuzzleAdminListResponse {
    code: PuzzleAdminResult,
    puzzles: Vec<RbPuzzleAdminData>,
}

#[derive(Serialize)]
struct PuzzleAdminDeleteResponse {
    code: PuzzleAdminResult,
}

fn validate_json_shape(data: &RbPuzzleCreateData) -> bool {
    data.judge.is_object() || data.judge.is_array()
}

fn validate_ptype(value: i16) -> bool {
    matches!(
        RbPuzzleType::from(value),
        RbPuzzleType::Normal | RbPuzzleType::Story
    )
}

fn validate_content_type(value: i16) -> bool {
    matches!(
        RbContentType::from(value),
        RbContentType::Markdown | RbContentType::Html | RbContentType::UnsafeMarkdown
    )
}

fn validate_unlock_cond(value: &str) -> bool {
    value == "default" || expr::compile_gate_expr(value).is_ok()
}

fn validate_slug(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn validate_slug_option(value: &Option<String>) -> bool {
    value.as_deref().is_none_or(validate_slug)
}

fn validate_create(data: &RbPuzzleCreateData) -> bool {
    validate_ptype(data.ptype)
        && validate_content_type(data.content_type)
        && validate_slug_option(&data.slug)
        && data.ticket_cooldown >= 0
        && data.max_submit.is_none_or(|value| value >= 0)
        && validate_unlock_cond(&data.unlock_cond)
        && validate_json_shape(data)
        && data.penalty.is_array()
}

fn validate_update(data: &RbPuzzleUpdateData) -> bool {
    if let Some(ptype) = data.ptype
        && !validate_ptype(ptype)
    {
        return false;
    }

    if let Some(content_type) = data.content_type
        && !validate_content_type(content_type)
    {
        return false;
    }

    if let Some(slug) = &data.slug
        && !validate_slug_option(slug)
    {
        return false;
    }

    if let Some(ticket_cooldown) = data.ticket_cooldown
        && ticket_cooldown < 0
    {
        return false;
    }

    if let Some(Some(max_submit)) = data.max_submit
        && max_submit < 0
    {
        return false;
    }

    if let Some(judge) = &data.judge
        && !(judge.is_object() || judge.is_array())
    {
        return false;
    }

    if let Some(penalty) = &data.penalty
        && !penalty.is_array()
    {
        return false;
    }

    if let Some(unlock_cond) = &data.unlock_cond
        && !validate_unlock_cond(unlock_cond)
    {
        return false;
    }

    true
}

async fn invalidate_puzzle_cache(app: &AppState, game_id: i32, puzzle_id: i32) {
    db::puzzle::invalidate_admin_cache(game_id, puzzle_id);

    if let Ok(mut conn) = app.kv.get().await {
        let _: Result<(), _> = conn.del(format!("puzzle:{puzzle_id}:show")).await;
    }
}

async fn list(
    query: web::Query<PuzzleListQuery>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let puzzles = db::puzzle::admin_list(&app.db, query.game_id).await?;

    Ok(HttpResponse::Ok().json(PuzzleAdminListResponse {
        code: PuzzleAdminResult::Ok,
        puzzles,
    }))
}

async fn get(path: web::Path<PuzzlePathInfo>, app: web::Data<AppState>) -> Result<HttpResponse> {
    let puzzle = db::puzzle::admin_get(&app.db, path.puzzle_id).await?;
    let Some(puzzle) = puzzle else {
        return RbError::not_found()
            .code(PuzzleAdminResult::NotFound.into())
            .http_err();
    };

    Ok(HttpResponse::Ok().json(PuzzleAdminResponse {
        code: PuzzleAdminResult::Ok,
        puzzle,
    }))
}

async fn append(
    req: web::Json<RbPuzzleCreateData>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !validate_create(&req) {
        return RbError::bad_req(PuzzleAdminResult::Invalid.into()).http_err();
    }

    let puzzle = match db::puzzle::admin_create(&app.db, &req).await {
        Ok(puzzle) => puzzle,
        Err(err) => {
            if is_constraint_error(&err) {
                return RbError::bad_req(PuzzleAdminResult::Invalid.into()).http_err();
            }
            return Err(err.into());
        }
    };
    let Some(puzzle) = puzzle else {
        return RbError::not_found()
            .code(PuzzleAdminResult::NotFound.into())
            .http_err();
    };
    invalidate_puzzle_cache(&app, puzzle.game_id, puzzle.id).await;

    Ok(HttpResponse::Ok().json(PuzzleAdminResponse {
        code: PuzzleAdminResult::Ok,
        puzzle,
    }))
}

async fn edit(
    path: web::Path<PuzzlePathInfo>,
    req: web::Json<RbPuzzleUpdateData>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !validate_update(&req) {
        return RbError::bad_req(PuzzleAdminResult::Invalid.into()).http_err();
    }

    let puzzle = match db::puzzle::admin_update(&app.db, path.puzzle_id, &req).await {
        Ok(puzzle) => puzzle,
        Err(err) => {
            if is_constraint_error(&err) {
                return RbError::bad_req(PuzzleAdminResult::Invalid.into()).http_err();
            }
            return Err(err.into());
        }
    };
    let Some(puzzle) = puzzle else {
        return RbError::not_found()
            .code(PuzzleAdminResult::NotFound.into())
            .http_err();
    };
    invalidate_puzzle_cache(&app, puzzle.game_id, path.puzzle_id).await;

    Ok(HttpResponse::Ok().json(PuzzleAdminResponse {
        code: PuzzleAdminResult::Ok,
        puzzle,
    }))
}

async fn delete(path: web::Path<PuzzlePathInfo>, app: web::Data<AppState>) -> Result<HttpResponse> {
    let puzzle = db::puzzle::admin_get(&app.db, path.puzzle_id).await?;
    let Some(puzzle) = puzzle else {
        return RbError::not_found()
            .code(PuzzleAdminResult::NotFound.into())
            .http_err();
    };

    let deleted = db::puzzle::admin_delete(&app.db, path.puzzle_id).await?;
    if !deleted {
        return RbError::not_found()
            .code(PuzzleAdminResult::NotFound.into())
            .http_err();
    }
    invalidate_puzzle_cache(&app, puzzle.game_id, path.puzzle_id).await;

    Ok(HttpResponse::Ok().json(PuzzleAdminDeleteResponse {
        code: PuzzleAdminResult::Ok,
    }))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("puzzles")
            .route("", web::get().to(list))
            .route("", web::post().to(append))
            .route("/{puzzle_id}", web::get().to(get))
            .route("/{puzzle_id}", web::patch().to(edit))
            .route("/{puzzle_id}", web::delete().to(delete)),
    );
}
