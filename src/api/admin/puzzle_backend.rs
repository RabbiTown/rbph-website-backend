use actix_web::{HttpResponse, Result, web};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{
    AppState,
    db::{
        self,
        puzzle_backend::{BackendScope, PuzzleBackendInput},
    },
    error::RbError,
};

#[derive(Deserialize)]
struct PuzzleBackendPathInfo {
    puzzle_id: i32,
}

#[derive(Deserialize)]
struct PuzzleBackendKvQuery {
    team_id: Option<i32>,
    prefix: Option<String>,
}

#[derive(Deserialize)]
struct PuzzleBackendKvPathInfo {
    puzzle_id: i32,
    key: String,
}

#[derive(Deserialize)]
struct PuzzleBackendLogQuery {
    execution_type: Option<String>,
    function_name: Option<String>,
    ok: Option<bool>,
    team_id: Option<i32>,
    user_id: Option<i32>,
    #[serde(
        default,
        with = "crate::serde_helpers::serialize_option_offset_datetime"
    )]
    from: Option<OffsetDateTime>,
    #[serde(
        default,
        with = "crate::serde_helpers::serialize_option_offset_datetime"
    )]
    to: Option<OffsetDateTime>,
    page: Option<i64>,
    limit: Option<i64>,
}

#[derive(Deserialize)]
struct PuzzleBackendSourceInput {
    source: String,
}

#[derive(Deserialize)]
struct PuzzleBackendFunctionsInput {
    functions: Vec<String>,
}

#[derive(Serialize)]
struct PuzzleBackendResponse {
    code: i32,
    backend: Option<db::puzzle_backend::PuzzleBackend>,
}

#[derive(Serialize)]
struct PuzzleBackendKvResponse {
    code: i32,
    entries: Vec<db::puzzle_backend::PuzzleBackendKvEntry>,
}

#[derive(Serialize)]
struct PuzzleBackendDeleteResponse {
    code: i32,
    deleted: bool,
}

#[derive(Serialize)]
struct PuzzleBackendLogResponse {
    code: i32,
    logs: Vec<db::puzzle_backend::PuzzleBackendCallLog>,
    total: i64,
}

async fn puzzle_exists(app: &AppState, puzzle_id: i32) -> Result<bool> {
    Ok(db::puzzle::admin_get(&app.db, puzzle_id).await?.is_some())
}

async fn get_backend(
    path: web::Path<PuzzleBackendPathInfo>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !puzzle_exists(&app, path.puzzle_id).await? {
        return RbError::not_found().http_err();
    }

    let backend = db::puzzle_backend::get_backend(&app.db, path.puzzle_id).await?;
    Ok(HttpResponse::Ok().json(PuzzleBackendResponse { code: 0, backend }))
}

async fn upsert_backend(
    path: web::Path<PuzzleBackendPathInfo>,
    req: web::Json<PuzzleBackendInput>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !puzzle_exists(&app, path.puzzle_id).await? {
        return RbError::not_found().http_err();
    }
    if req.source.trim().is_empty() {
        return RbError::bad_req(-2).http_err();
    }

    let backend = db::puzzle_backend::upsert_backend(&app.db, path.puzzle_id, &req).await?;
    Ok(HttpResponse::Ok().json(PuzzleBackendResponse {
        code: 0,
        backend: Some(backend),
    }))
}

async fn update_backend_source(
    path: web::Path<PuzzleBackendPathInfo>,
    req: web::Json<PuzzleBackendSourceInput>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !puzzle_exists(&app, path.puzzle_id).await? {
        return RbError::not_found().http_err();
    }
    if req.source.trim().is_empty() {
        return RbError::bad_req(-2).http_err();
    }

    let backend =
        db::puzzle_backend::update_backend_source(&app.db, path.puzzle_id, &req.source).await?;
    Ok(HttpResponse::Ok().json(PuzzleBackendResponse {
        code: 0,
        backend: Some(backend),
    }))
}

async fn update_backend_functions(
    path: web::Path<PuzzleBackendPathInfo>,
    req: web::Json<PuzzleBackendFunctionsInput>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !puzzle_exists(&app, path.puzzle_id).await? {
        return RbError::not_found().http_err();
    }

    let mut functions = Vec::new();
    for name in &req.functions {
        if !db::puzzle_backend::is_valid_backend_function_name(name) {
            return RbError::bad_req(-2).http_err();
        }
        if !functions.iter().any(|item| item == name) {
            functions.push(name.clone());
        }
    }

    let backend =
        db::puzzle_backend::update_backend_functions(&app.db, path.puzzle_id, &functions).await?;

    Ok(HttpResponse::Ok().json(PuzzleBackendResponse {
        code: 0,
        backend: Some(backend),
    }))
}

async fn delete_backend(
    path: web::Path<PuzzleBackendPathInfo>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !puzzle_exists(&app, path.puzzle_id).await? {
        return RbError::not_found().http_err();
    }

    let deleted = db::puzzle_backend::delete_backend(&app.db, path.puzzle_id).await?;
    Ok(HttpResponse::Ok().json(PuzzleBackendDeleteResponse { code: 0, deleted }))
}

async fn list_kv(
    path: web::Path<PuzzleBackendPathInfo>,
    query: web::Query<PuzzleBackendKvQuery>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !puzzle_exists(&app, path.puzzle_id).await? {
        return RbError::not_found().http_err();
    }

    let Some(puzzle) = db::puzzle::admin_get(&app.db, path.puzzle_id).await? else {
        return RbError::not_found().http_err();
    };
    let scope = if let Some(team_id) = query.team_id {
        BackendScope::TeamPuzzle {
            team_id,
            puzzle_id: path.puzzle_id,
        }
    } else {
        BackendScope::Puzzle {
            puzzle_id: path.puzzle_id,
        }
    };
    let entries =
        db::puzzle_backend::list_kv(&app.db, puzzle.game_id, scope, query.prefix.as_deref())
            .await?;

    Ok(HttpResponse::Ok().json(PuzzleBackendKvResponse { code: 0, entries }))
}

async fn delete_kv(
    path: web::Path<PuzzleBackendKvPathInfo>,
    query: web::Query<PuzzleBackendKvQuery>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !puzzle_exists(&app, path.puzzle_id).await? {
        return RbError::not_found().http_err();
    }
    let Some(puzzle) = db::puzzle::admin_get(&app.db, path.puzzle_id).await? else {
        return RbError::not_found().http_err();
    };
    let scope = if let Some(team_id) = query.team_id {
        BackendScope::TeamPuzzle {
            team_id,
            puzzle_id: path.puzzle_id,
        }
    } else {
        BackendScope::Puzzle {
            puzzle_id: path.puzzle_id,
        }
    };
    let deleted = db::puzzle_backend::delete_kv(&app.db, puzzle.game_id, scope, &path.key).await?;

    Ok(HttpResponse::Ok().json(PuzzleBackendDeleteResponse { code: 0, deleted }))
}

async fn list_logs(
    path: web::Path<PuzzleBackendPathInfo>,
    query: web::Query<PuzzleBackendLogQuery>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !puzzle_exists(&app, path.puzzle_id).await? {
        return RbError::not_found().http_err();
    }

    let execution_type = query
        .execution_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if execution_type.is_some_and(|value| !matches!(value, "api" | "judge" | "hint_purchase")) {
        return RbError::bad_req(-2).http_err();
    }
    let function_name = query
        .function_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if query.from.zip(query.to).is_some_and(|(from, to)| from > to) {
        return RbError::bad_req(-2).http_err();
    }

    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let page = query.page.unwrap_or(1).max(1);
    let log_query = db::puzzle_backend::PuzzleBackendCallLogQuery {
        puzzle_id: path.puzzle_id,
        execution_type,
        function_name,
        ok: query.ok,
        team_id: query.team_id,
        user_id: query.user_id,
        from: query.from,
        to: query.to,
        offset: (page - 1).saturating_mul(limit),
        limit,
    };
    let total = db::puzzle_backend::count_call_logs(&app.db, &log_query).await?;
    let logs = db::puzzle_backend::list_call_logs(&app.db, log_query).await?;

    Ok(HttpResponse::Ok().json(PuzzleBackendLogResponse {
        code: 0,
        logs,
        total,
    }))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("puzzles/{puzzle_id}/backend")
            .route("", web::get().to(get_backend))
            .route("", web::put().to(upsert_backend))
            .route("/source", web::patch().to(update_backend_source))
            .route("/functions", web::patch().to(update_backend_functions))
            .route("", web::delete().to(delete_backend))
            .route("/logs", web::get().to(list_logs))
            .route("/kv", web::get().to(list_kv))
            .route("/kv/{key}", web::delete().to(delete_kv)),
    );
}
