use actix_web::{HttpResponse, Result, web};
use serde::{Deserialize, Serialize};

use crate::{
    AppState,
    db::{self, puzzle_backend::PuzzleBackendInput},
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
        if name.trim().is_empty() || name.len() > 64 {
            return RbError::bad_req(-2).http_err();
        }
        if !name.chars().enumerate().all(|(index, c)| {
            c == '_'
                || c.is_ascii_alphanumeric()
                || c == '-'
                || index == 0 && c.is_ascii_alphabetic()
        }) {
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

    let entries = db::puzzle_backend::list_kv(
        &app.db,
        path.puzzle_id,
        query.team_id,
        query.prefix.as_deref(),
    )
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
    let deleted =
        db::puzzle_backend::delete_kv(&app.db, path.puzzle_id, query.team_id, &path.key).await?;

    Ok(HttpResponse::Ok().json(PuzzleBackendDeleteResponse { code: 0, deleted }))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("puzzles/{puzzle_id}/backend")
            .route("", web::get().to(get_backend))
            .route("", web::put().to(upsert_backend))
            .route("/source", web::patch().to(update_backend_source))
            .route("/functions", web::patch().to(update_backend_functions))
            .route("", web::delete().to(delete_backend))
            .route("/kv", web::get().to(list_kv))
            .route("/kv/{key}", web::delete().to(delete_kv)),
    );
}
