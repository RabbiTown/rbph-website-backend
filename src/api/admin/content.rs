use std::collections::{HashMap, HashSet};

use actix_web::{HttpResponse, Result, web};
use serde::{Deserialize, Serialize};

use crate::{
    AppState,
    db::{self, content::RbContentBlockAdminData},
    error::{RbError, RbInternalError},
    expr,
    model::game::RbContentType,
};

const CONTENT_CDN_RELATIVE_PATH: &str = "body.txt";
const CONTENT_CDN_MIME_TYPE: &str = "text/plain; charset=utf-8";
const CONTENT_CDN_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

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

struct PreparedArtifact {
    backend: String,
    object_key: String,
    relative_path: String,
    sha256: String,
    size: i64,
}

struct PreparedBlockUpdate<'a> {
    request: &'a UpdateBlockRequest,
    update_artifact: bool,
    artifact: Option<PreparedArtifact>,
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

async fn upload_content_artifact(
    app: &AppState,
    backend: &str,
    content: &str,
) -> Result<PreparedArtifact, RbInternalError> {
    let object_key = format!("content-{}", uuid::Uuid::new_v4());
    let stored = app
        .storage
        .store_public_file(
            backend,
            &object_key,
            CONTENT_CDN_RELATIVE_PATH,
            content.as_bytes(),
            CONTENT_CDN_MIME_TYPE,
            Some(CONTENT_CDN_CACHE_CONTROL),
        )
        .await?;
    Ok(PreparedArtifact {
        backend: backend.to_string(),
        object_key,
        relative_path: CONTENT_CDN_RELATIVE_PATH.to_string(),
        sha256: stored.sha256,
        size: stored.size.try_into().unwrap_or(i64::MAX),
    })
}

async fn delete_content_artifact(
    app: &AppState,
    artifact: &db::content::ContentBlockArtifactDelete,
) {
    if let Err(error) = app
        .storage
        .delete_files(
            &artifact.backend,
            &artifact.object_key,
            std::slice::from_ref(&artifact.relative_path),
        )
        .await
    {
        log::warn!(
            "failed to delete content CDN artifact {}/{} from {}: {}",
            artifact.object_key,
            artifact.relative_path,
            artifact.backend,
            error
        );
    }
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
    let current = db::content::admin_list(&app.db, puzzle_id, round_id).await?;
    let current_by_id = current
        .into_iter()
        .map(|block| (block.id, block))
        .collect::<HashMap<_, _>>();
    let owned = current_by_id.keys().copied().collect::<HashSet<_>>();
    let requested = request
        .blocks
        .iter()
        .map(|block| block.id)
        .collect::<HashSet<_>>();
    if requested.len() != request.blocks.len() || !requested.is_subset(&owned) {
        return RbError::bad_req(-2).http_err();
    }

    let content_cdn_backend = app
        .settings
        .storage
        .content_cdn_backend
        .as_deref()
        .filter(|backend| app.storage.supports_public_url(backend));
    let mut uploaded = Vec::new();
    let mut stale = Vec::new();
    let mut prepared = Vec::with_capacity(request.blocks.len());
    for block in &request.blocks {
        let current = current_by_id
            .get(&block.id)
            .ok_or_else(RbError::not_found)?;
        let content_changed =
            current.content != block.content || current.content_type != block.content_type;
        let artifact = if content_changed {
            if let Some(old) = current.artifact_delete() {
                stale.push(old);
            }
            if let Some(backend) = content_cdn_backend
                && !block.content.is_empty()
            {
                let artifact = upload_content_artifact(app, backend, &block.content).await?;
                uploaded.push(db::content::ContentBlockArtifactDelete {
                    backend: artifact.backend.clone(),
                    object_key: artifact.object_key.clone(),
                    relative_path: artifact.relative_path.clone(),
                });
                Some(artifact)
            } else {
                None
            }
        } else {
            None
        };
        prepared.push(PreparedBlockUpdate {
            request: block,
            update_artifact: content_changed,
            artifact,
        });
    }

    let mut tx = app.db.begin().await.map_err(RbInternalError::from)?;
    let update_result = async {
        for block in &prepared {
            let artifact =
                block
                    .artifact
                    .as_ref()
                    .map(|artifact| db::content::ContentBlockArtifact {
                        backend: &artifact.backend,
                        object_key: &artifact.object_key,
                        relative_path: &artifact.relative_path,
                        sha256: &artifact.sha256,
                        size: artifact.size,
                    });
            if db::content::admin_update(
                &mut tx,
                block.request.id,
                db::content::ContentBlockUpdate {
                    name: block.request.name.trim(),
                    content: &block.request.content,
                    content_type: block.request.content_type,
                    visibility_cond: &block.request.visibility_cond,
                    update_artifact: block.update_artifact,
                    artifact,
                },
            )
            .await?
            .is_none()
            {
                return Err(RbError::not_found().code(-1).into());
            }
        }
        tx.commit()
            .await
            .map_err(RbInternalError::from)
            .map_err(actix_web::Error::from)?;
        Ok::<(), actix_web::Error>(())
    }
    .await;
    if let Err(error) = update_result {
        for artifact in &uploaded {
            delete_content_artifact(app, artifact).await;
        }
        return Err(error);
    }
    for artifact in &stale {
        delete_content_artifact(app, artifact).await;
    }
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
    let artifact = db::content::admin_get(&app.db, path.block_id)
        .await?
        .and_then(|block| block.artifact_delete());
    if !db::content::admin_delete(&app.db, path.block_id).await? {
        return RbError::not_found().code(-1).http_err();
    }
    if let Some(artifact) = &artifact {
        delete_content_artifact(&app, artifact).await;
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
