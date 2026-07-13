use actix_multipart::Multipart;
use std::collections::HashMap;

use actix_web::{HttpResponse, Result, http::header, web};
use futures_util::StreamExt;
use num_enum::IntoPrimitive;
use serde::{Deserialize, Serialize};
use serde_repr::Serialize_repr;
use sha2::{Digest, Sha256};

use crate::{
    AppState,
    db::{
        self,
        asset::{RbAssetFileAdminData, RbAssetGroupAdminData, RbAssetGroupWithFilesAdminData},
    },
    error::{RbError, RbInternalError},
    module::storage::{
        AssetUploadFile, DATABASE_MAX_FILE_BYTES, DATABASE_MAX_GROUP_BYTES,
        DATABASE_MAX_GROUP_FILES, LocalStorage, StoredAssetFile, StoredAssetGroup,
        build_public_path, sanitize_relative_path, uniquify_relative_path,
    },
};

const MAX_MULTIPART_FILE_BYTES: usize = 64 * 1024 * 1024;

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
    group: AssetGroupData,
    files: Vec<AssetFileData>,
}

#[derive(Serialize)]
struct AssetAdminListResponse {
    code: AssetAdminResult,
    groups: Vec<AssetGroupItemData>,
}

#[derive(Serialize)]
struct AssetGroupData {
    #[serde(flatten)]
    group: RbAssetGroupAdminData,
    public_url: Option<String>,
}

#[derive(Serialize)]
struct AssetFileData {
    #[serde(flatten)]
    file: RbAssetFileAdminData,
    public_url: Option<String>,
}

#[derive(Serialize)]
struct AssetGroupItemData {
    group: AssetGroupData,
    files: Vec<AssetFileData>,
}

#[derive(Serialize)]
struct AssetStorageBackendData {
    backend: String,
    kind: &'static str,
    label: String,
    recommended: bool,
    public_read: bool,
    backend_read: bool,
    allowed_scopes: &'static [&'static str],
    max_file_bytes: Option<u64>,
    max_group_bytes: Option<u64>,
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
    group: Option<AssetGroupData>,
    files: Vec<AssetFileData>,
}

fn asset_group_data(app: &AppState, group: RbAssetGroupAdminData) -> AssetGroupData {
    let public_url = app
        .storage
        .asset_group_public_url(&group.backend, &group.object_key);
    AssetGroupData { group, public_url }
}

fn asset_file_data(
    app: &AppState,
    group: &RbAssetGroupAdminData,
    file: RbAssetFileAdminData,
) -> AssetFileData {
    let public_url =
        app.storage
            .asset_public_url(&group.backend, &group.object_key, &file.relative_path);
    AssetFileData { file, public_url }
}

fn asset_files_data(
    app: &AppState,
    group: &RbAssetGroupAdminData,
    files: Vec<RbAssetFileAdminData>,
) -> Vec<AssetFileData> {
    files
        .into_iter()
        .map(|file| asset_file_data(app, group, file))
        .collect()
}

fn asset_response_data(
    app: &AppState,
    group: RbAssetGroupAdminData,
    files: Vec<RbAssetFileAdminData>,
) -> AssetAdminResponse {
    let files = asset_files_data(app, &group, files);
    AssetAdminResponse {
        code: AssetAdminResult::Ok,
        group: asset_group_data(app, group),
        files,
    }
}

fn asset_group_item_data(
    app: &AppState,
    item: RbAssetGroupWithFilesAdminData,
) -> AssetGroupItemData {
    let files = asset_files_data(app, &item.group, item.files);
    AssetGroupItemData {
        group: asset_group_data(app, item.group),
        files,
    }
}

fn asset_file_delete_response(
    app: &AppState,
    deleted_group: bool,
    group: Option<RbAssetGroupAdminData>,
    files: Vec<RbAssetFileAdminData>,
) -> AssetAdminFileDeleteResponse {
    let files = group
        .as_ref()
        .map(|group| asset_files_data(app, group, files))
        .unwrap_or_default();
    AssetAdminFileDeleteResponse {
        code: AssetAdminResult::Ok,
        deleted_group,
        group: group.map(|group| asset_group_data(app, group)),
        files,
    }
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

fn prepare_database_group(
    object_key: &str,
    files: Vec<AssetUploadFile>,
) -> (StoredAssetGroup, Vec<Vec<u8>>) {
    let mut stored_files = Vec::with_capacity(files.len());
    let mut contents = Vec::with_capacity(files.len());
    let mut used_paths = HashMap::new();
    let mut group_hasher = Sha256::new();
    let mut group_size = 0_u64;

    for file in files {
        let relative_path =
            uniquify_relative_path(sanitize_relative_path(&file.relative_path), &mut used_paths);
        let size = file.bytes.len() as u64;
        let sha256 = format!("{:x}", Sha256::digest(&file.bytes));
        group_hasher.update(relative_path.as_bytes());
        group_hasher.update([0]);
        group_hasher.update(size.to_le_bytes());
        group_hasher.update(&file.bytes);
        group_size += size;
        stored_files.push(StoredAssetFile {
            relative_path: relative_path.clone(),
            size,
            sha256,
            mime_type: file.mime_type,
            path: build_public_path(object_key, &relative_path),
        });
        contents.push(file.bytes);
    }

    (
        StoredAssetGroup {
            size: group_size,
            sha256: format!("{:x}", group_hasher.finalize()),
            files: stored_files,
        },
        contents,
    )
}

fn summarize_database_files(files: &[(String, Vec<u8>)]) -> (u64, String) {
    let mut hasher = Sha256::new();
    let mut size = 0_u64;
    for (relative_path, content) in files {
        let file_size = content.len() as u64;
        hasher.update(relative_path.as_bytes());
        hasher.update([0]);
        hasher.update(file_size.to_le_bytes());
        hasher.update(content);
        size += file_size;
    }
    (size, format!("{:x}", hasher.finalize()))
}

async fn list(query: web::Query<AssetListQuery>, app: web::Data<AppState>) -> Result<HttpResponse> {
    if query.puzzle_id.is_some() && query.round_id.is_some() {
        return RbError::bad_req(AssetAdminResult::Invalid.into()).http_err();
    }

    let groups =
        db::asset::list_by_scope(&app.db, query.game_id, query.puzzle_id, query.round_id).await?;
    Ok(HttpResponse::Ok().json(AssetAdminListResponse {
        code: AssetAdminResult::Ok,
        groups: groups
            .into_iter()
            .map(|group| asset_group_item_data(&app, group))
            .collect(),
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
                public_read: backend.public_read,
                backend_read: backend.backend_read,
                allowed_scopes: backend.allowed_scopes,
                max_file_bytes: backend.max_file_bytes,
                max_group_bytes: backend.max_group_bytes,
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
                let chunk = chunk?;
                if bytes.len().saturating_add(chunk.len()) > MAX_MULTIPART_FILE_BYTES {
                    return RbError::bad_req(AssetAdminResult::Invalid.into()).http_err();
                }
                bytes.extend_from_slice(&chunk);
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
    let database_backend = app.storage.is_database(&backend);
    if database_backend && round_id.is_some() {
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
        let (max_file, max_group, max_files) = if database_backend {
            (
                DATABASE_MAX_FILE_BYTES,
                DATABASE_MAX_GROUP_BYTES,
                DATABASE_MAX_GROUP_FILES,
            )
        } else {
            (
                MAX_MULTIPART_FILE_BYTES as u64,
                MAX_MULTIPART_FILE_BYTES as u64,
                DATABASE_MAX_GROUP_FILES,
            )
        };
        let unpacked =
            LocalStorage::unpack_zip_files_limited(&file_bytes, max_file, max_group, max_files)
                .map_err(|_| RbError::bad_req(AssetAdminResult::Invalid.into()))?;
        if unpacked.is_empty() {
            return RbError::bad_req(AssetAdminResult::Invalid.into()).http_err();
        }
        unpacked
    } else {
        if database_backend && file_bytes.len() as u64 > DATABASE_MAX_FILE_BYTES {
            return RbError::bad_req(AssetAdminResult::Invalid.into()).http_err();
        }
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

    if database_backend {
        let (stored, contents) = prepare_database_group(&object_key, files);
        if stored.size > DATABASE_MAX_GROUP_BYTES
            || stored.files.len() > DATABASE_MAX_GROUP_FILES
            || stored
                .files
                .iter()
                .any(|file| file.size > DATABASE_MAX_FILE_BYTES)
        {
            return RbError::bad_req(AssetAdminResult::Invalid.into()).http_err();
        }

        let mut tx = app.db.begin().await.map_err(RbInternalError::from)?;
        let group = db::asset::create_group_conn(
            &mut tx,
            db::asset::CreateAssetGroupData {
                game_id,
                puzzle_id,
                round_id: None,
                backend: &backend,
                object_key: &object_key,
                original_name: &group_name,
                mime_type: &group_mime,
                size: stored.size as i64,
                sha256: &stored.sha256,
            },
        )
        .await?;
        let mut db_files = Vec::with_capacity(stored.files.len());
        for (stored_file, content) in stored.files.iter().zip(&contents) {
            let file = db::asset::create_file_conn(
                &mut tx,
                group.id,
                &stored_file.relative_path,
                &stored_file.mime_type,
                stored_file.size as i64,
                &stored_file.sha256,
            )
            .await?;
            db::asset::create_file_blob_conn(&mut tx, file.id, content).await?;
            db_files.push(file);
        }
        tx.commit().await.map_err(RbInternalError::from)?;
        return Ok(HttpResponse::Ok().json(asset_response_data(&app, group, db_files)));
    }

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
        let group = db::asset::create_group_conn(
            &mut tx,
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
                db::asset::create_file_conn(
                    &mut tx,
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

    Ok(HttpResponse::Ok().json(asset_response_data(&app, group, db_files)))
}

async fn recompute_group_metadata(
    app: &AppState,
    group: RbAssetGroupAdminData,
    files: &[RbAssetFileAdminData],
) -> Result<RbAssetGroupAdminData> {
    if app.storage.is_database(&group.backend) {
        let blobs = db::asset::list_file_blobs(&app.db, group.id).await?;
        let (size, sha256) = summarize_database_files(&blobs);
        return db::asset::admin_update_group_metadata(&app.db, group.id, size as i64, &sha256)
            .await?
            .ok_or_else(|| {
                RbError::not_found()
                    .code(AssetAdminResult::NotFound.into())
                    .into()
            });
    }
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

async fn recompute_database_group_metadata_conn(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: i32,
) -> Result<RbAssetGroupAdminData> {
    let blobs = db::asset::list_file_blobs_conn(&mut *tx, group_id).await?;
    let (size, sha256) = summarize_database_files(&blobs);
    db::asset::admin_update_group_metadata(&mut **tx, group_id, size as i64, &sha256)
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

    Ok(HttpResponse::Ok().json(asset_response_data(&app, group, files)))
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

            if app.storage.is_database(&group.backend) {
                let mut tx = app.db.begin().await.map_err(RbInternalError::from)?;
                if db::asset::admin_update_file_path(
                    &mut *tx,
                    path.group_id,
                    path.file_id,
                    &relative_path,
                )
                .await?
                .is_none()
                {
                    return RbError::not_found()
                        .code(AssetAdminResult::NotFound.into())
                        .http_err();
                }
                let group = recompute_database_group_metadata_conn(&mut tx, group.id).await?;
                tx.commit().await.map_err(RbInternalError::from)?;
                let files = db::asset::list_files(&app.db, group.id).await?;
                return Ok(HttpResponse::Ok().json(asset_response_data(&app, group, files)));
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

            return Ok(HttpResponse::Ok().json(asset_response_data(&app, group, files)));
        }
    }

    let files = db::asset::list_files(&app.db, group.id).await?;
    Ok(HttpResponse::Ok().json(asset_response_data(&app, group, files)))
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
        return Ok(HttpResponse::Ok().json(asset_response_data(&app, group, files)));
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

    if app.storage.is_database(&group.backend) {
        let mut tx = app.db.begin().await.map_err(RbInternalError::from)?;
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
                return RbError::not_found()
                    .code(AssetAdminResult::NotFound.into())
                    .http_err();
            }
        }
        let group = recompute_database_group_metadata_conn(&mut tx, group.id).await?;
        tx.commit().await.map_err(RbInternalError::from)?;
        let files = db::asset::list_files(&app.db, group.id).await?;
        return Ok(HttpResponse::Ok().json(asset_response_data(&app, group, files)));
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

    Ok(HttpResponse::Ok().json(asset_response_data(&app, group, files)))
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
        if !app.storage.is_database(&group.backend) {
            app.storage
                .delete_files(&group.backend, &group.object_key, &paths)
                .await
                .map_err(|_| RbError::internal("failed to remove asset group files"))?;
        }

        return Ok(HttpResponse::Ok().json(asset_file_delete_response(
            &app,
            true,
            None,
            Vec::new(),
        )));
    }

    if !app.storage.is_database(&group.backend) {
        app.storage
            .delete_files(
                &group.backend,
                &group.object_key,
                std::slice::from_ref(&file.relative_path),
            )
            .await
            .map_err(|_| RbError::internal("failed to remove asset file"))?;
    }

    if app.storage.is_database(&group.backend) {
        let mut tx = app.db.begin().await.map_err(RbInternalError::from)?;
        if !db::asset::admin_delete_file(&mut *tx, group.id, file.id).await? {
            return RbError::not_found()
                .code(AssetAdminResult::NotFound.into())
                .http_err();
        }
        let group = recompute_database_group_metadata_conn(&mut tx, group.id).await?;
        tx.commit().await.map_err(RbInternalError::from)?;
        let files = db::asset::list_files(&app.db, group.id).await?;
        return Ok(HttpResponse::Ok().json(asset_file_delete_response(
            &app,
            false,
            Some(group),
            files,
        )));
    }

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

    Ok(HttpResponse::Ok().json(asset_file_delete_response(&app, false, Some(group), files)))
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
    if !app.storage.is_database(&group.backend) {
        app.storage
            .delete_files(&group.backend, &group.object_key, &paths)
            .await
            .map_err(|_| RbError::internal("failed to remove asset group files"))?;
    }

    Ok(HttpResponse::Ok().json(AssetAdminDeleteResponse {
        code: AssetAdminResult::Ok,
    }))
}

async fn download_file(
    path: web::Path<AssetFilePathInfo>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let group = db::asset::admin_get_group(&app.db, path.group_id)
        .await?
        .filter(|group| app.storage.is_database(&group.backend))
        .ok_or_else(|| RbError::not_found().code(AssetAdminResult::NotFound.into()))?;
    let file = db::asset::admin_get_file(&app.db, group.id, path.file_id)
        .await?
        .ok_or_else(|| RbError::not_found().code(AssetAdminResult::NotFound.into()))?;
    let content = db::asset::get_file_blob_by_id(&app.db, group.id, file.id)
        .await?
        .ok_or_else(|| RbError::not_found().code(AssetAdminResult::NotFound.into()))?;

    Ok(HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, file.mime_type))
        .insert_header((header::CACHE_CONTROL, "no-store"))
        .insert_header((
            header::CONTENT_DISPOSITION,
            format!(
                "attachment; filename*=UTF-8''{}",
                percent_encoding::utf8_percent_encode(
                    &file.relative_path,
                    percent_encoding::NON_ALPHANUMERIC
                )
            ),
        ))
        .body(content))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("assets")
            .route("/storage-backends", web::get().to(storage_backends))
            .route("", web::get().to(list))
            .route("", web::post().to(append))
            .route("/{group_id}", web::patch().to(patch))
            .route("/{group_id}/files/{file_id}", web::patch().to(patch_file))
            .route(
                "/{group_id}/files/{file_id}/content",
                web::get().to(download_file),
            )
            .route("/{group_id}/files/{file_id}", web::delete().to(delete_file))
            .route("/{group_id}/folders", web::patch().to(patch_folder))
            .route("/{group_id}", web::delete().to(delete)),
    );
}
