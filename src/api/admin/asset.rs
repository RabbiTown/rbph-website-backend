use actix_multipart::Multipart;
use actix_web::{HttpResponse, Result, web};
use futures_util::StreamExt;
use num_enum::IntoPrimitive;
use serde::{Deserialize, Serialize};
use serde_repr::Serialize_repr;

use crate::{
    AppState,
    db::{
        self,
        asset::{RbAssetFileAdminData, RbAssetGroupAdminData, RbAssetGroupWithFilesAdminData},
    },
    error::{RbError, RbInternalError},
    module::storage::{AssetUploadFile, LocalStorage, StoredAssetGroup, sanitize_relative_path},
};

#[derive(Deserialize)]
struct AssetListQuery {
    game_id: i32,
    puzzle_id: Option<i32>,
    round_id: Option<i32>,
}

#[derive(Deserialize)]
struct AssetPathInfo {
    group_id: i32,
}

#[derive(Deserialize)]
struct AssetFilePathInfo {
    group_id: i32,
    file_id: i32,
}

#[derive(Deserialize)]
struct AssetPatchRequest {
    original_name: Option<String>,
}

#[derive(Deserialize)]
struct AssetFilePatchRequest {
    file_name: Option<String>,
}

#[derive(Deserialize)]
struct AssetFolderPatchRequest {
    path: String,
    name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UploadMode {
    File,
    Group,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum AssetAdminResult {
    Invalid = -2,
    NotFound = -1,
    Ok = 0,
}

#[derive(Serialize)]
struct AssetAdminResponse {
    code: AssetAdminResult,
    group: RbAssetGroupAdminData,
    files: Vec<RbAssetFileAdminData>,
}

#[derive(Serialize)]
struct AssetAdminListResponse {
    code: AssetAdminResult,
    groups: Vec<RbAssetGroupWithFilesAdminData>,
}

#[derive(Serialize)]
struct AssetStorageBackendData {
    backend: String,
    kind: &'static str,
    label: String,
    recommended: bool,
}

#[derive(Serialize)]
struct AssetStorageBackendResponse {
    code: AssetAdminResult,
    backends: Vec<AssetStorageBackendData>,
}

#[derive(Serialize)]
struct AssetAdminDeleteResponse {
    code: AssetAdminResult,
}

#[derive(Serialize)]
struct AssetAdminFileDeleteResponse {
    code: AssetAdminResult,
    deleted_group: bool,
    group: Option<RbAssetGroupAdminData>,
    files: Vec<RbAssetFileAdminData>,
}

fn valid_asset_group_name(name: &str) -> bool {
    !name.trim().is_empty() && name.chars().count() <= 255
}

fn normalize_asset_relative_path(path: &str) -> Option<String> {
    let path = path.trim();
    if path.is_empty() || path.chars().count() > 1024 {
        return None;
    }

    let normalized = sanitize_relative_path(path);
    if normalized == path {
        Some(normalized)
    } else {
        None
    }
}

fn normalize_asset_path_segment(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 255 || name.contains('/') || name.contains('\\') {
        return None;
    }

    let normalized = sanitize_relative_path(name);
    if normalized == name {
        Some(normalized)
    } else {
        None
    }
}

fn parent_path(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(parent, _)| parent)
}

fn join_asset_path(parent: Option<&str>, name: &str) -> String {
    parent.map_or_else(|| name.to_string(), |parent| format!("{parent}/{name}"))
}

fn is_path_in_folder(path: &str, folder: &str) -> bool {
    path.strip_prefix(folder)
        .is_some_and(|suffix| suffix.starts_with('/'))
}

async fn list(query: web::Query<AssetListQuery>, app: web::Data<AppState>) -> Result<HttpResponse> {
    if query.puzzle_id.is_some() && query.round_id.is_some() {
        return RbError::bad_req(AssetAdminResult::Invalid.into()).http_err();
    }

    let groups =
        db::asset::list_by_scope(&app.db, query.game_id, query.puzzle_id, query.round_id).await?;
    Ok(HttpResponse::Ok().json(AssetAdminListResponse {
        code: AssetAdminResult::Ok,
        groups,
    }))
}

async fn storage_backends(app: web::Data<AppState>) -> HttpResponse {
    HttpResponse::Ok().json(AssetStorageBackendResponse {
        code: AssetAdminResult::Ok,
        backends: app
            .storage
            .available_backends()
            .into_iter()
            .map(|backend| AssetStorageBackendData {
                backend: backend.id,
                kind: backend.kind,
                label: backend.label,
                recommended: backend.recommended,
            })
            .collect(),
    })
}

async fn append(mut payload: Multipart, app: web::Data<AppState>) -> Result<HttpResponse> {
    let mut game_id: Option<i32> = None;
    let mut puzzle_id: Option<i32> = None;
    let mut round_id: Option<i32> = None;
    let mut backend: Option<String> = None;
    let mut mode = UploadMode::File;
    let mut file_name: Option<String> = None;
    let mut file_mime: Option<String> = None;
    let mut file_bytes: Option<Vec<u8>> = None;

    while let Some(field) = payload.next().await {
        let mut field = field?;
        let name = field.name().unwrap_or_default().to_string();
        let content_type = field.content_type().map(|v| v.to_string());
        let disposition_name = field
            .content_disposition()
            .and_then(|d| d.get_filename().map(|s| s.to_string()));

        if name == "file" {
            let mut bytes = Vec::new();
            while let Some(chunk) = field.next().await {
                bytes.extend_from_slice(&chunk?);
            }
            if file_bytes.is_some() {
                return RbError::bad_req(AssetAdminResult::Invalid.into()).http_err();
            }
            file_name = Some(disposition_name.unwrap_or_else(|| "file".to_string()));
            file_mime =
                Some(content_type.unwrap_or_else(|| "application/octet-stream".to_string()));
            file_bytes = Some(bytes);
            continue;
        }

        if name == "mode" {
            let mut bytes = Vec::new();
            while let Some(chunk) = field.next().await {
                bytes.extend_from_slice(&chunk?);
            }
            let text = String::from_utf8(bytes).unwrap_or_default();
            mode = match text.trim() {
                "group" => UploadMode::Group,
                "file" | "" => UploadMode::File,
                _ => return RbError::bad_req(AssetAdminResult::Invalid.into()).http_err(),
            };
            continue;
        }

        let mut buf = Vec::new();
        while let Some(chunk) = field.next().await {
            buf.extend_from_slice(&chunk?);
        }
        let text = String::from_utf8(buf).unwrap_or_default();
        match name.as_str() {
            "game_id" => game_id = text.trim().parse::<i32>().ok(),
            "puzzle_id" => puzzle_id = text.trim().parse::<i32>().ok(),
            "round_id" => round_id = text.trim().parse::<i32>().ok(),
            "backend" => backend = Some(text.trim().to_string()),
            _ => {}
        }
    }

    let Some(game_id) = game_id else {
        return RbError::bad_req(AssetAdminResult::Invalid.into()).http_err();
    };
    let Some(backend) = backend.filter(|value| app.storage.has_backend(value)) else {
        return RbError::bad_req(AssetAdminResult::Invalid.into()).http_err();
    };
    if !db::game::exists(&app.db, game_id, crate::model::user::RbUserRole::Admin).await? {
        return RbError::not_found()
            .code(AssetAdminResult::NotFound.into())
            .http_err();
    }
    if puzzle_id.is_some() && round_id.is_some() {
        return RbError::bad_req(AssetAdminResult::Invalid.into()).http_err();
    }
    if let Some(puzzle_id) = puzzle_id {
        let Some(puzzle_game_id) = db::puzzle::get_puzzle_game(&app.db, puzzle_id).await? else {
            return RbError::not_found()
                .code(AssetAdminResult::NotFound.into())
                .http_err();
        };
        if puzzle_game_id != game_id {
            return RbError::bad_req(AssetAdminResult::Invalid.into()).http_err();
        }
    }
    if let Some(round_id) = round_id {
        let Some(round) = db::round::get_round_game(&app.db, round_id).await? else {
            return RbError::not_found()
                .code(AssetAdminResult::NotFound.into())
                .http_err();
        };
        if round != game_id {
            return RbError::bad_req(AssetAdminResult::Invalid.into()).http_err();
        }
    }
    let Some(file_bytes) = file_bytes else {
        return RbError::bad_req(AssetAdminResult::Invalid.into()).http_err();
    };
    let file_name = file_name.unwrap_or_else(|| "file".to_string());
    let file_mime = file_mime.unwrap_or_else(|| "application/octet-stream".to_string());

    let files = if matches!(mode, UploadMode::Group) {
        let is_zip =
            file_mime == "application/zip" || file_name.to_ascii_lowercase().ends_with(".zip");
        if !is_zip {
            return RbError::bad_req(AssetAdminResult::Invalid.into()).http_err();
        }
        let unpacked = LocalStorage::unpack_zip_files(&file_bytes)
            .map_err(|_| RbError::bad_req(AssetAdminResult::Invalid.into()))?;
        if unpacked.is_empty() {
            return RbError::bad_req(AssetAdminResult::Invalid.into()).http_err();
        }
        unpacked
    } else {
        vec![AssetUploadFile {
            relative_path: file_name.clone(),
            bytes: file_bytes,
            mime_type: file_mime.clone(),
        }]
    };

    let object_key = format!("group-{}", uuid::Uuid::new_v4());
    let group_name = file_name.clone();
    let group_mime = if mode == UploadMode::Group {
        "application/zip".to_string()
    } else {
        file_mime.clone()
    };

    let StoredAssetGroup {
        size: group_size,
        sha256: group_sha256,
        files: stored_files,
    } = app
        .storage
        .store_group_files(&backend, &object_key, &files)
        .await
        .map_err(|_| RbError::bad_req(AssetAdminResult::Invalid.into()))?;

    let mut tx = app.db.begin().await.map_err(RbInternalError::from)?;
    let result = async {
        let group = db::asset::create_group(
            &mut *tx,
            db::asset::CreateAssetGroupData {
                game_id,
                puzzle_id,
                round_id,
                backend: &backend,
                object_key: &object_key,
                original_name: &group_name,
                mime_type: &group_mime,
                size: group_size as i64,
                sha256: &group_sha256,
            },
        )
        .await?;

        let mut db_files = Vec::with_capacity(stored_files.len());
        for file in &stored_files {
            db_files.push(
                db::asset::create_file(
                    &mut *tx,
                    group.id,
                    &file.relative_path,
                    &file.mime_type,
                    file.size as i64,
                    &file.sha256,
                )
                .await?,
            );
        }
        Ok::<_, crate::error::RbInternalError>((group, db_files))
    }
    .await;

    let (group, db_files) = match result {
        Ok(value) => {
            tx.commit().await.map_err(RbInternalError::from)?;
            value
        }
        Err(err) => {
            let paths = stored_files
                .iter()
                .map(|file| file.relative_path.clone())
                .collect::<Vec<_>>();
            let _ = app
                .storage
                .delete_files(&backend, &object_key, &paths)
                .await;
            return Err(err.into());
        }
    };

    Ok(HttpResponse::Ok().json(AssetAdminResponse {
        code: AssetAdminResult::Ok,
        group,
        files: db_files,
    }))
}

async fn recompute_group_metadata(
    app: &AppState,
    group: RbAssetGroupAdminData,
    files: &[RbAssetFileAdminData],
) -> Result<RbAssetGroupAdminData> {
    let paths = files
        .iter()
        .map(|file| file.relative_path.clone())
        .collect::<Vec<_>>();
    let summary = app
        .storage
        .summarize_existing_group_files(&group.backend, &group.object_key, &paths)
        .await
        .map_err(|_| RbError::internal("failed to summarize asset group files"))?;

    db::asset::admin_update_group_metadata(&app.db, group.id, summary.size as i64, &summary.sha256)
        .await?
        .ok_or_else(|| {
            RbError::not_found()
                .code(AssetAdminResult::NotFound.into())
                .into()
        })
}

async fn patch(
    path: web::Path<AssetPathInfo>,
    body: web::Json<AssetPatchRequest>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let mut group = db::asset::admin_get_group(&app.db, path.group_id)
        .await?
        .ok_or_else(|| RbError::not_found().code(AssetAdminResult::NotFound.into()))?;

    if let Some(original_name) = &body.original_name {
        let original_name = original_name.trim();
        if !valid_asset_group_name(original_name) {
            return RbError::bad_req(AssetAdminResult::Invalid.into()).http_err();
        }

        group = db::asset::admin_update_group_name(&app.db, path.group_id, original_name)
            .await?
            .ok_or_else(|| RbError::not_found().code(AssetAdminResult::NotFound.into()))?;
    }

    let files = db::asset::list_files(&app.db, group.id).await?;

    Ok(HttpResponse::Ok().json(AssetAdminResponse {
        code: AssetAdminResult::Ok,
        group,
        files,
    }))
}

async fn patch_file(
    path: web::Path<AssetFilePathInfo>,
    body: web::Json<AssetFilePatchRequest>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let group = db::asset::admin_get_group(&app.db, path.group_id)
        .await?
        .ok_or_else(|| RbError::not_found().code(AssetAdminResult::NotFound.into()))?;
    let file = db::asset::admin_get_file(&app.db, path.group_id, path.file_id)
        .await?
        .ok_or_else(|| RbError::not_found().code(AssetAdminResult::NotFound.into()))?;

    if let Some(file_name) = &body.file_name {
        let Some(file_name) = normalize_asset_path_segment(file_name) else {
            return RbError::bad_req(AssetAdminResult::Invalid.into()).http_err();
        };
        let relative_path = join_asset_path(parent_path(&file.relative_path), &file_name);

        if relative_path != file.relative_path {
            if db::asset::admin_file_path_exists(
                &app.db,
                path.group_id,
                &relative_path,
                path.file_id,
            )
            .await?
            {
                return RbError::bad_req(AssetAdminResult::Invalid.into()).http_err();
            }

            app.storage
                .rename_file(
                    &group.backend,
                    &group.object_key,
                    &file.relative_path,
                    &relative_path,
                    &file.mime_type,
                )
                .await
                .map_err(|_| RbError::internal("failed to rename asset file"))?;

            let updated_file = match db::asset::admin_update_file_path(
                &app.db,
                path.group_id,
                path.file_id,
                &relative_path,
            )
            .await
            {
                Ok(Some(file)) => file,
                Ok(None) => {
                    let _ = app
                        .storage
                        .rename_file(
                            &group.backend,
                            &group.object_key,
                            &relative_path,
                            &file.relative_path,
                            &file.mime_type,
                        )
                        .await;
                    return RbError::not_found()
                        .code(AssetAdminResult::NotFound.into())
                        .http_err();
                }
                Err(error) => {
                    let _ = app
                        .storage
                        .rename_file(
                            &group.backend,
                            &group.object_key,
                            &relative_path,
                            &file.relative_path,
                            &file.mime_type,
                        )
                        .await;
                    return Err(error.into());
                }
            };

            let mut files = db::asset::list_files(&app.db, group.id).await?;
            for current in &mut files {
                if current.id == updated_file.id {
                    *current = updated_file.clone();
                }
            }
            let group = recompute_group_metadata(&app, group, &files).await?;

            return Ok(HttpResponse::Ok().json(AssetAdminResponse {
                code: AssetAdminResult::Ok,
                group,
                files,
            }));
        }
    }

    let files = db::asset::list_files(&app.db, group.id).await?;
    Ok(HttpResponse::Ok().json(AssetAdminResponse {
        code: AssetAdminResult::Ok,
        group,
        files,
    }))
}

async fn patch_folder(
    path: web::Path<AssetPathInfo>,
    body: web::Json<AssetFolderPatchRequest>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let group = db::asset::admin_get_group(&app.db, path.group_id)
        .await?
        .ok_or_else(|| RbError::not_found().code(AssetAdminResult::NotFound.into()))?;
    let Some(folder_path) = normalize_asset_relative_path(&body.path) else {
        return RbError::bad_req(AssetAdminResult::Invalid.into()).http_err();
    };
    let Some(folder_name) = normalize_asset_path_segment(&body.name) else {
        return RbError::bad_req(AssetAdminResult::Invalid.into()).http_err();
    };

    let new_folder_path = join_asset_path(parent_path(&folder_path), &folder_name);
    if new_folder_path == folder_path {
        let files = db::asset::list_files(&app.db, group.id).await?;
        return Ok(HttpResponse::Ok().json(AssetAdminResponse {
            code: AssetAdminResult::Ok,
            group,
            files,
        }));
    }

    let files = db::asset::list_files(&app.db, group.id).await?;
    let affected = files
        .iter()
        .filter(|file| is_path_in_folder(&file.relative_path, &folder_path))
        .cloned()
        .collect::<Vec<_>>();
    if affected.is_empty() {
        return RbError::not_found()
            .code(AssetAdminResult::NotFound.into())
            .http_err();
    }

    if files
        .iter()
        .filter(|file| !is_path_in_folder(&file.relative_path, &folder_path))
        .any(|file| {
            file.relative_path == new_folder_path
                || is_path_in_folder(&file.relative_path, &new_folder_path)
        })
    {
        return RbError::bad_req(AssetAdminResult::Invalid.into()).http_err();
    }

    let mut renamed: Vec<(String, String, String)> = Vec::new();
    for file in &affected {
        let suffix = file
            .relative_path
            .strip_prefix(&folder_path)
            .unwrap_or_default();
        let relative_path = format!("{new_folder_path}{suffix}");
        if app
            .storage
            .rename_file(
                &group.backend,
                &group.object_key,
                &file.relative_path,
                &relative_path,
                &file.mime_type,
            )
            .await
            .is_err()
        {
            for (old_path, new_path, mime_type) in renamed.iter().rev() {
                let _ = app
                    .storage
                    .rename_file(
                        &group.backend,
                        &group.object_key,
                        new_path,
                        old_path,
                        mime_type,
                    )
                    .await;
            }
            return Err(RbError::internal("failed to rename asset folder").into());
        }
        renamed.push((
            file.relative_path.clone(),
            relative_path,
            file.mime_type.clone(),
        ));
    }

    let mut tx = app.db.begin().await.map_err(RbInternalError::from)?;
    let mut update_failed = false;
    for file in &affected {
        let suffix = file
            .relative_path
            .strip_prefix(&folder_path)
            .unwrap_or_default();
        let relative_path = format!("{new_folder_path}{suffix}");
        if db::asset::admin_update_file_path(&mut *tx, group.id, file.id, &relative_path)
            .await?
            .is_none()
        {
            update_failed = true;
            break;
        }
    }

    if update_failed {
        let _ = tx.rollback().await;
        for (old_path, new_path, mime_type) in renamed.iter().rev() {
            let _ = app
                .storage
                .rename_file(
                    &group.backend,
                    &group.object_key,
                    new_path,
                    old_path,
                    mime_type,
                )
                .await;
        }
        return RbError::not_found()
            .code(AssetAdminResult::NotFound.into())
            .http_err();
    }

    if let Err(error) = tx.commit().await {
        for (old_path, new_path, mime_type) in renamed.iter().rev() {
            let _ = app
                .storage
                .rename_file(
                    &group.backend,
                    &group.object_key,
                    new_path,
                    old_path,
                    mime_type,
                )
                .await;
        }
        return Err(RbInternalError::from(error).into());
    }
    let files = db::asset::list_files(&app.db, group.id).await?;
    let group = recompute_group_metadata(&app, group, &files).await?;

    Ok(HttpResponse::Ok().json(AssetAdminResponse {
        code: AssetAdminResult::Ok,
        group,
        files,
    }))
}

async fn delete_file(
    path: web::Path<AssetFilePathInfo>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let group = db::asset::admin_get_group(&app.db, path.group_id)
        .await?
        .ok_or_else(|| RbError::not_found().code(AssetAdminResult::NotFound.into()))?;
    let file = db::asset::admin_get_file(&app.db, path.group_id, path.file_id)
        .await?
        .ok_or_else(|| RbError::not_found().code(AssetAdminResult::NotFound.into()))?;
    let files = db::asset::list_files(&app.db, group.id).await?;

    if files.len() <= 1 {
        let mut tx = app.db.begin().await.map_err(RbInternalError::from)?;
        let deleted = db::asset::admin_delete_group(&mut *tx, group.id).await?;
        if !deleted {
            return RbError::not_found()
                .code(AssetAdminResult::NotFound.into())
                .http_err();
        }
        tx.commit().await.map_err(RbInternalError::from)?;

        let paths = files
            .iter()
            .map(|file| file.relative_path.clone())
            .collect::<Vec<_>>();
        app.storage
            .delete_files(&group.backend, &group.object_key, &paths)
            .await
            .map_err(|_| RbError::internal("failed to remove asset group files"))?;

        return Ok(HttpResponse::Ok().json(AssetAdminFileDeleteResponse {
            code: AssetAdminResult::Ok,
            deleted_group: true,
            group: None,
            files: Vec::new(),
        }));
    }

    app.storage
        .delete_files(
            &group.backend,
            &group.object_key,
            std::slice::from_ref(&file.relative_path),
        )
        .await
        .map_err(|_| RbError::internal("failed to remove asset file"))?;

    match db::asset::admin_delete_file(&app.db, group.id, file.id).await {
        Ok(true) => {}
        Ok(false) => {
            return RbError::not_found()
                .code(AssetAdminResult::NotFound.into())
                .http_err();
        }
        Err(error) => return Err(error.into()),
    }

    let files = db::asset::list_files(&app.db, group.id).await?;
    let group = recompute_group_metadata(&app, group, &files).await?;

    Ok(HttpResponse::Ok().json(AssetAdminFileDeleteResponse {
        code: AssetAdminResult::Ok,
        deleted_group: false,
        group: Some(group),
        files,
    }))
}

async fn delete(path: web::Path<AssetPathInfo>, app: web::Data<AppState>) -> Result<HttpResponse> {
    let Some(group) = db::asset::admin_get_group(&app.db, path.group_id).await? else {
        return RbError::not_found()
            .code(AssetAdminResult::NotFound.into())
            .http_err();
    };
    let files = db::asset::list_files(&app.db, group.id).await?;

    let mut tx = app.db.begin().await.map_err(RbInternalError::from)?;
    let deleted = db::asset::admin_delete_group(&mut *tx, path.group_id).await?;
    if !deleted {
        return RbError::not_found()
            .code(AssetAdminResult::NotFound.into())
            .http_err();
    }
    tx.commit().await.map_err(RbInternalError::from)?;

    let paths = files
        .iter()
        .map(|file| file.relative_path.clone())
        .collect::<Vec<_>>();
    app.storage
        .delete_files(&group.backend, &group.object_key, &paths)
        .await
        .map_err(|_| RbError::internal("failed to remove asset group files"))?;

    Ok(HttpResponse::Ok().json(AssetAdminDeleteResponse {
        code: AssetAdminResult::Ok,
    }))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("assets")
            .route("/storage-backends", web::get().to(storage_backends))
            .route("", web::get().to(list))
            .route("", web::post().to(append))
            .route("/{group_id}", web::patch().to(patch))
            .route("/{group_id}/files/{file_id}", web::patch().to(patch_file))
            .route("/{group_id}/files/{file_id}", web::delete().to(delete_file))
            .route("/{group_id}/folders", web::patch().to(patch_folder))
            .route("/{group_id}", web::delete().to(delete)),
    );
}
