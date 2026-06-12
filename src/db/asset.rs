use serde::Serialize;
use sqlx::prelude::FromRow;
use time::OffsetDateTime;

use sqlx::{Postgres, Executor};

use crate::{DbPool, error::RbInternalError};

#[derive(FromRow, Serialize)]
pub struct RbAssetGroupAdminData {
    pub id: i32,
    pub game_id: i32,
    pub puzzle_id: Option<i32>,
    pub backend: String,
    pub object_key: String,
    pub original_name: String,
    pub mime_type: String,
    pub size: i64,
    pub sha256: String,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub ctime_at: OffsetDateTime,
}

#[derive(FromRow, Serialize, Clone)]
pub struct RbAssetFileAdminData {
    pub id: i32,
    pub group_id: i32,
    pub relative_path: String,
    pub mime_type: String,
    pub size: i64,
    pub sha256: String,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub ctime_at: OffsetDateTime,
}

#[derive(Serialize)]
pub struct RbAssetGroupWithFilesAdminData {
    pub group: RbAssetGroupAdminData,
    pub files: Vec<RbAssetFileAdminData>,
}

pub async fn list_by_scope(
    pool: &DbPool,
    game_id: i32,
    puzzle_id: Option<i32>,
) -> Result<Vec<RbAssetGroupWithFilesAdminData>, RbInternalError> {
    let groups = if let Some(puzzle_id) = puzzle_id {
        sqlx::query_as!(
            RbAssetGroupAdminData,
            "SELECT id, game_id, puzzle_id, backend, object_key, original_name, mime_type, size, sha256, ctime_at
            FROM rb_asset_group
            WHERE game_id = $1 AND puzzle_id = $2
            ORDER BY ctime_at DESC, id DESC;",
            game_id,
            puzzle_id
        )
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as!(
            RbAssetGroupAdminData,
            "SELECT id, game_id, puzzle_id, backend, object_key, original_name, mime_type, size, sha256, ctime_at
            FROM rb_asset_group
            WHERE game_id = $1 AND puzzle_id IS NULL
            ORDER BY ctime_at DESC, id DESC;",
            game_id
        )
        .fetch_all(pool)
        .await?
    };

    let mut result = Vec::with_capacity(groups.len());
    for group in groups {
        let files = list_files(pool, group.id).await?;
        result.push(RbAssetGroupWithFilesAdminData { group, files });
    }
    Ok(result)
}

pub async fn list_files(pool: &DbPool, group_id: i32) -> Result<Vec<RbAssetFileAdminData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbAssetFileAdminData,
        "SELECT id, group_id, relative_path, mime_type, size, sha256, ctime_at
        FROM rb_asset_file
        WHERE group_id = $1
        ORDER BY relative_path ASC, id ASC;",
        group_id
    )
    .fetch_all(pool)
    .await?;

    Ok(result)
}

pub async fn create_group<'e, E>(
    executor: E,
    game_id: i32,
    puzzle_id: Option<i32>,
    backend: &str,
    object_key: &str,
    original_name: &str,
    mime_type: &str,
    size: i64,
    sha256: &str,
)
-> Result<RbAssetGroupAdminData, RbInternalError>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query_as!(
        RbAssetGroupAdminData,
        "INSERT INTO rb_asset_group (
            game_id, puzzle_id, backend, object_key, original_name, mime_type, size, sha256
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        RETURNING id, game_id, puzzle_id, backend, object_key, original_name, mime_type, size, sha256, ctime_at;",
        game_id,
        puzzle_id,
        backend,
        object_key,
        original_name,
        mime_type,
        size,
        sha256
    )
    .fetch_one(executor)
    .await?;

    Ok(result)
}

pub async fn create_file<'e, E>(
    executor: E,
    group_id: i32,
    relative_path: &str,
    mime_type: &str,
    size: i64,
    sha256: &str,
) -> Result<RbAssetFileAdminData, RbInternalError>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query_as!(
        RbAssetFileAdminData,
        "INSERT INTO rb_asset_file (
            group_id, relative_path, mime_type, size, sha256
        )
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id, group_id, relative_path, mime_type, size, sha256, ctime_at;",
        group_id,
        relative_path,
        mime_type,
        size,
        sha256
    )
    .fetch_one(executor)
    .await?;

    Ok(result)
}

pub async fn admin_get_group(pool: &DbPool, group_id: i32) -> Result<Option<RbAssetGroupAdminData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbAssetGroupAdminData,
        "SELECT id, game_id, puzzle_id, backend, object_key, original_name, mime_type, size, sha256, ctime_at
        FROM rb_asset_group
        WHERE id = $1;",
        group_id
    )
    .fetch_optional(pool)
    .await?;

    Ok(result)
}

pub async fn admin_delete_group<'e, E>(executor: E, group_id: i32) -> Result<bool, RbInternalError>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        "DELETE FROM rb_asset_group
        WHERE id = $1;",
        group_id
    )
    .execute(executor)
    .await?;

    Ok(result.rows_affected() > 0)
}
