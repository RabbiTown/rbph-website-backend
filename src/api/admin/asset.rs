use actix_multipart::Multipart;
use actix_web::{HttpResponse, Result, web};
use futures_util::StreamExt;
use num_enum::IntoPrimitive;
use serde::{Deserialize, Serialize};
use serde_repr::Serialize_repr;

use crate::{
    AppState,
    db::{self, asset::{RbAssetFileAdminData, RbAssetGroupAdminData, RbAssetGroupWithFilesAdminData}},
    error::{RbError, RbInternalError},
    module::storage::{AssetUploadFile, LocalStorage, StoredAssetGroup},
};

#[derive(Deserialize)]
struct AssetListQuery {
    game_id: i32,
    puzzle_id: Option<i32>,
}

#[derive(Deserialize)]
struct AssetPathInfo {
    group_id: i32,
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
struct AssetAdminDeleteResponse {
    code: AssetAdminResult,
}

async fn list(query: web::Query<AssetListQuery>, app: web::Data<AppState>) -> Result<HttpResponse> {
    let groups = db::asset::list_by_scope(&app.db, query.game_id, query.puzzle_id).await?;
    Ok(HttpResponse::Ok().json(AssetAdminListResponse {
        code: AssetAdminResult::Ok,
        groups,
    }))
}

async fn append(mut payload: Multipart, app: web::Data<AppState>) -> Result<HttpResponse> {
    let mut game_id: Option<i32> = None;
    let mut puzzle_id: Option<i32> = None;
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
            file_mime = Some(content_type.unwrap_or_else(|| "application/octet-stream".to_string()));
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
            _ => {}
        }
    }

    let Some(game_id) = game_id else {
        return RbError::bad_req(AssetAdminResult::Invalid.into()).http_err();
    };
    if let Some(puzzle_id) = puzzle_id {
        let Some(puzzle_game_id) = db::puzzle::get_puzzle_game(&app.db, puzzle_id).await? else {
            return RbError::not_found().code(AssetAdminResult::NotFound.into()).http_err();
        };
        if puzzle_game_id != game_id {
            return RbError::bad_req(AssetAdminResult::Invalid.into()).http_err();
        }
    }
    let Some(file_bytes) = file_bytes else {
        return RbError::bad_req(AssetAdminResult::Invalid.into()).http_err();
    };
    let file_name = file_name.unwrap_or_else(|| "file".to_string());
    let file_mime = file_mime.unwrap_or_else(|| "application/octet-stream".to_string());

    let files = if matches!(mode, UploadMode::Group) {
        let is_zip = file_mime == "application/zip" || file_name.to_ascii_lowercase().ends_with(".zip");
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

    let StoredAssetGroup { size: group_size, sha256: group_sha256, files: stored_files } = app
        .storage
        .store_group_files(&object_key, &files)
        .await
        .map_err(|_| RbError::bad_req(AssetAdminResult::Invalid.into()))?;

    let mut tx = app.db.begin().await.map_err(RbInternalError::from)?;
    let result = async {
        let group = db::asset::create_group(
            &mut *tx,
            game_id,
            puzzle_id,
            "local",
            &object_key,
            &group_name,
            &group_mime,
            group_size as i64,
            &group_sha256,
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
            let _ = tokio::fs::remove_dir_all(app.storage.object_dir(&object_key)).await;
            return Err(err.into());
        }
    };

    Ok(HttpResponse::Ok().json(AssetAdminResponse {
        code: AssetAdminResult::Ok,
        group,
        files: db_files,
    }))
}

async fn delete(path: web::Path<AssetPathInfo>, app: web::Data<AppState>) -> Result<HttpResponse> {
    let Some(group) = db::asset::admin_get_group(&app.db, path.group_id).await? else {
        return RbError::not_found()
            .code(AssetAdminResult::NotFound.into())
            .http_err();
    };

    let mut tx = app.db.begin().await.map_err(RbInternalError::from)?;
    let deleted = db::asset::admin_delete_group(&mut *tx, path.group_id).await?;
    if !deleted {
        return RbError::not_found()
            .code(AssetAdminResult::NotFound.into())
            .http_err();
    }
    tx.commit().await.map_err(RbInternalError::from)?;

    let file_path = app.storage.object_dir(&group.object_key);
    tokio::fs::remove_dir_all(&file_path)
        .await
        .map_err(|_| RbError::internal("failed to remove asset group files"))?;

    Ok(HttpResponse::Ok().json(AssetAdminDeleteResponse {
        code: AssetAdminResult::Ok,
    }))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("assets")
            .route("", web::get().to(list))
            .route("", web::post().to(append))
            .route("/{group_id}", web::delete().to(delete)),
    );
}
