use std::collections::HashSet;

use actix_web::{HttpResponse, Result, web};
use serde::{Deserialize, Serialize};

use crate::{
    AppState,
    db::{self, content::RbContentBlockAdminData},
    error::{RbError, RbInternalError},
    expr,
    model::game::RbContentType,
};

#[derive(Deserialize)]
struct OwnerPath {
    owner_id: i32,
}

#[derive(Deserialize)]
struct BlockPath {
    block_id: i32,
}

#[derive(Deserialize)]
struct ReorderRequest {
    ids: Vec<i32>,
}

#[derive(Deserialize)]
struct CreateRequest {
    name: String,
}

#[derive(Deserialize)]
struct UpdateBlockRequest {
    id: i32,
    name: String,
    content: String,
    content_type: i16,
    visibility_cond: String,
}

#[derive(Deserialize)]
struct UpdateRequest {
    blocks: Vec<UpdateBlockRequest>,
}

#[derive(Serialize)]
struct ListResponse {
    code: i32,
    blocks: Vec<RbContentBlockAdminData>,
}

#[derive(Serialize)]
struct BlockResponse {
    code: i32,
    block: RbContentBlockAdminData,
}

#[derive(Serialize)]
struct OkResponse {
    code: i32,
}

fn valid_content_type(value: i16) -> bool {
    matches!(
        RbContentType::from(value),
        RbContentType::Markdown | RbContentType::Html | RbContentType::UnsafeMarkdown
    )
}

fn valid_condition(value: &str) -> bool {
    value == "default" || expr::compile_gate_expr(value).is_ok()
}

async fn list_owner(
    app: &AppState,
    puzzle_id: Option<i32>,
    round_id: Option<i32>,
) -> Result<HttpResponse> {
    let blocks = db::content::admin_list(&app.db, puzzle_id, round_id).await?;
    Ok(HttpResponse::Ok().json(ListResponse { code: 0, blocks }))
}

async fn create_owner(
    app: &AppState,
    puzzle_id: Option<i32>,
    round_id: Option<i32>,
    name: &str,
) -> Result<HttpResponse> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 120 {
        return RbError::bad_req(-2).http_err();
    }
    let Some(block) = db::content::admin_create(&app.db, puzzle_id, round_id, name).await? else {
        return RbError::not_found().code(-1).http_err();
    };
    Ok(HttpResponse::Ok().json(BlockResponse { code: 0, block }))
}

async fn reorder_owner(
    app: &AppState,
    puzzle_id: Option<i32>,
    round_id: Option<i32>,
    ids: &[i32],
) -> Result<HttpResponse> {
    if !db::content::admin_reorder(&app.db, puzzle_id, round_id, ids).await? {
        return RbError::bad_req(-2).http_err();
    }
    Ok(HttpResponse::Ok().json(OkResponse { code: 0 }))
}

async fn update_owner(
    app: &AppState,
    puzzle_id: Option<i32>,
    round_id: Option<i32>,
    request: &UpdateRequest,
) -> Result<HttpResponse> {
    if request.blocks.iter().any(|block| {
        block.name.trim().is_empty()
            || block.name.chars().count() > 120
            || !valid_content_type(block.content_type)
            || !valid_condition(&block.visibility_cond)
    }) {
        return RbError::bad_req(-2).http_err();
    }
    let owned = db::content::admin_list(&app.db, puzzle_id, round_id)
        .await?
        .into_iter()
        .map(|block| block.id)
        .collect::<HashSet<_>>();
    let requested = request
        .blocks
        .iter()
        .map(|block| block.id)
        .collect::<HashSet<_>>();
    if requested.len() != request.blocks.len() || !requested.is_subset(&owned) {
        return RbError::bad_req(-2).http_err();
    }

    let mut tx = app.db.begin().await.map_err(RbInternalError::from)?;
    for block in &request.blocks {
        if db::content::admin_update(
            &mut tx,
            block.id,
            block.name.trim(),
            &block.content,
            block.content_type,
            &block.visibility_cond,
        )
        .await?
        .is_none()
        {
            return RbError::not_found().code(-1).http_err();
        }
    }
    tx.commit().await.map_err(RbInternalError::from)?;
    let blocks = db::content::admin_list(&app.db, puzzle_id, round_id).await?;
    Ok(HttpResponse::Ok().json(ListResponse { code: 0, blocks }))
}

async fn puzzle_list(path: web::Path<OwnerPath>, app: web::Data<AppState>) -> Result<HttpResponse> {
    list_owner(&app, Some(path.owner_id), None).await
}

async fn puzzle_create(
    path: web::Path<OwnerPath>,
    req: web::Json<CreateRequest>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    create_owner(&app, Some(path.owner_id), None, &req.name).await
}

async fn puzzle_reorder(
    path: web::Path<OwnerPath>,
    req: web::Json<ReorderRequest>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    reorder_owner(&app, Some(path.owner_id), None, &req.ids).await
}

async fn puzzle_update(
    path: web::Path<OwnerPath>,
    req: web::Json<UpdateRequest>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    update_owner(&app, Some(path.owner_id), None, &req).await
}

async fn round_list(path: web::Path<OwnerPath>, app: web::Data<AppState>) -> Result<HttpResponse> {
    list_owner(&app, None, Some(path.owner_id)).await
}

async fn round_create(
    path: web::Path<OwnerPath>,
    req: web::Json<CreateRequest>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    create_owner(&app, None, Some(path.owner_id), &req.name).await
}

async fn round_reorder(
    path: web::Path<OwnerPath>,
    req: web::Json<ReorderRequest>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    reorder_owner(&app, None, Some(path.owner_id), &req.ids).await
}

async fn round_update(
    path: web::Path<OwnerPath>,
    req: web::Json<UpdateRequest>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    update_owner(&app, None, Some(path.owner_id), &req).await
}

async fn delete(path: web::Path<BlockPath>, app: web::Data<AppState>) -> Result<HttpResponse> {
    if !db::content::admin_delete(&app.db, path.block_id).await? {
        return RbError::not_found().code(-1).http_err();
    }
    Ok(HttpResponse::Ok().json(OkResponse { code: 0 }))
}

async fn clear_unlocks(
    path: web::Path<BlockPath>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    db::content::admin_clear_unlocks(&app.db, path.block_id).await?;
    Ok(HttpResponse::Ok().json(OkResponse { code: 0 }))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("puzzles/{owner_id}/content-blocks")
            .route("", web::get().to(puzzle_list))
            .route("", web::post().to(puzzle_create))
            .route("", web::patch().to(puzzle_update))
            .route("/order", web::put().to(puzzle_reorder)),
    )
    .service(
        web::scope("rounds/{owner_id}/content-blocks")
            .route("", web::get().to(round_list))
            .route("", web::post().to(round_create))
            .route("", web::patch().to(round_update))
            .route("/order", web::put().to(round_reorder)),
    )
    .service(
        web::scope("content-blocks/{block_id}")
            .route("", web::delete().to(delete))
            .route("/unlocks", web::delete().to(clear_unlocks)),
    );
}
