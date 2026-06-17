use serde::Serialize;
use sqlx::prelude::FromRow;
use time::OffsetDateTime;

use sqlx::{Executor, Postgres};

use crate::{DbPool, error::RbInternalError};

#[derive(FromRow, Serialize)]
pub struct RbAssetGroupAdminData {
    pub id: i32,
    pub game_id: i32,
    pub puzzle_id: Option<i32>,
    pub round_id: Option<i32>,
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

#[derive(Clone, Serialize)]
pub struct RbAssetReadableFile {
    pub group_id: i32,
    pub object_key: String,
    pub original_name: String,
    pub relative_path: String,
    pub mime_type: String,
    pub size: i64,
    pub sha256: String,
}

#[derive(Serialize)]
pub struct RbAssetGroupWithFilesAdminData {
    pub group: RbAssetGroupAdminData,
    pub files: Vec<RbAssetFileAdminData>,
}

pub struct CreateAssetGroupData<'a> {
    pub game_id: i32,
    pub puzzle_id: Option<i32>,
    pub round_id: Option<i32>,
    pub backend: &'a str,
    pub object_key: &'a str,
    pub original_name: &'a str,
    pub mime_type: &'a str,
    pub size: i64,
    pub sha256: &'a str,
}

pub async fn list_by_scope(
    pool: &DbPool,
    game_id: i32,
    puzzle_id: Option<i32>,
    round_id: Option<i32>,
) -> Result<Vec<RbAssetGroupWithFilesAdminData>, RbInternalError> {
    let groups = if let Some(puzzle_id) = puzzle_id {
        sqlx::query_as!(
            RbAssetGroupAdminData,
            "SELECT id, game_id, puzzle_id, round_id, backend, object_key, original_name, mime_type, size, sha256, ctime_at
            FROM rb_asset_group
            WHERE game_id = $1 AND puzzle_id = $2 AND round_id IS NULL
            ORDER BY ctime_at DESC, id DESC;",
            game_id,
            puzzle_id
        )
        .fetch_all(pool)
        .await?
    } else if let Some(round_id) = round_id {
        sqlx::query_as!(
            RbAssetGroupAdminData,
            "SELECT id, game_id, puzzle_id, round_id, backend, object_key, original_name, mime_type, size, sha256, ctime_at
            FROM rb_asset_group
            WHERE game_id = $1 AND round_id = $2 AND puzzle_id IS NULL
            ORDER BY ctime_at DESC, id DESC;",
            game_id,
            round_id
        )
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as!(
            RbAssetGroupAdminData,
            "SELECT id, game_id, puzzle_id, round_id, backend, object_key, original_name, mime_type, size, sha256, ctime_at
            FROM rb_asset_group
            WHERE game_id = $1 AND puzzle_id IS NULL AND round_id IS NULL
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

pub async fn list_files(
    pool: &DbPool,
    group_id: i32,
) -> Result<Vec<RbAssetFileAdminData>, RbInternalError> {
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

pub async fn list_readable_files_by_object_key(
    pool: &DbPool,
    game_id: i32,
    puzzle_id: i32,
    object_key: &str,
) -> Result<Vec<RbAssetReadableFile>, RbInternalError> {
    let result = sqlx::query_as!(
        RbAssetReadableFile,
        r#"SELECT f.group_id, g.object_key, g.original_name,
            f.relative_path, f.mime_type, f.size, f.sha256
        FROM rb_asset_file f
        JOIN rb_asset_group g ON g.id = f.group_id
        WHERE g.object_key = $1
            AND g.game_id = $2
            AND (
                (g.puzzle_id = $3 AND g.round_id IS NULL)
                OR (g.puzzle_id IS NULL AND g.round_id IS NULL)
            )
        ORDER BY f.relative_path ASC, f.id ASC;"#,
        object_key,
        game_id,
        puzzle_id
    )
    .fetch_all(pool)
    .await?;

    Ok(result)
}

pub async fn get_readable_file_by_object_key(
    pool: &DbPool,
    game_id: i32,
    puzzle_id: i32,
    object_key: &str,
    relative_path: &str,
) -> Result<Option<RbAssetReadableFile>, RbInternalError> {
    let result = sqlx::query_as!(
        RbAssetReadableFile,
        r#"SELECT f.group_id, g.object_key, g.original_name,
            f.relative_path, f.mime_type, f.size, f.sha256
        FROM rb_asset_file f
        JOIN rb_asset_group g ON g.id = f.group_id
        WHERE g.object_key = $1
            AND f.relative_path = $2
            AND g.game_id = $3
            AND (
                (g.puzzle_id = $4 AND g.round_id IS NULL)
                OR (g.puzzle_id IS NULL AND g.round_id IS NULL)
            );"#,
        object_key,
        relative_path,
        game_id,
        puzzle_id
    )
    .fetch_optional(pool)
    .await?;

    Ok(result)
}

pub async fn admin_get_file(
    pool: &DbPool,
    group_id: i32,
    file_id: i32,
) -> Result<Option<RbAssetFileAdminData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbAssetFileAdminData,
        "SELECT id, group_id, relative_path, mime_type, size, sha256, ctime_at
        FROM rb_asset_file
        WHERE group_id = $1 AND id = $2;",
        group_id,
        file_id
    )
    .fetch_optional(pool)
    .await?;

    Ok(result)
}

pub async fn admin_file_path_exists(
    pool: &DbPool,
    group_id: i32,
    relative_path: &str,
    except_file_id: i32,
) -> Result<bool, RbInternalError> {
    let result = sqlx::query_scalar!(
        "SELECT EXISTS (
            SELECT 1
            FROM rb_asset_file
            WHERE group_id = $1 AND relative_path = $2 AND id <> $3
        );",
        group_id,
        relative_path,
        except_file_id
    )
    .fetch_one(pool)
    .await?;

    Ok(result.unwrap_or(false))
}

pub async fn create_group<'e, E>(
    executor: E,
    data: CreateAssetGroupData<'_>,
) -> Result<RbAssetGroupAdminData, RbInternalError>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query_as!(
        RbAssetGroupAdminData,
        "INSERT INTO rb_asset_group (
            game_id, puzzle_id, round_id, backend, object_key, original_name, mime_type, size, sha256
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        RETURNING id, game_id, puzzle_id, round_id, backend, object_key, original_name, mime_type, size, sha256, ctime_at;",
        data.game_id,
        data.puzzle_id,
        data.round_id,
        data.backend,
        data.object_key,
        data.original_name,
        data.mime_type,
        data.size,
        data.sha256
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

pub async fn admin_get_group(
    pool: &DbPool,
    group_id: i32,
) -> Result<Option<RbAssetGroupAdminData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbAssetGroupAdminData,
        "SELECT id, game_id, puzzle_id, round_id, backend, object_key, original_name, mime_type, size, sha256, ctime_at
        FROM rb_asset_group
        WHERE id = $1;",
        group_id
    )
    .fetch_optional(pool)
    .await?;

    Ok(result)
}

pub async fn admin_update_group_name<'e, E>(
    executor: E,
    group_id: i32,
    original_name: &str,
) -> Result<Option<RbAssetGroupAdminData>, RbInternalError>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query_as!(
        RbAssetGroupAdminData,
        "UPDATE rb_asset_group
        SET original_name = $2
        WHERE id = $1
        RETURNING id, game_id, puzzle_id, round_id, backend, object_key, original_name, mime_type, size, sha256, ctime_at;",
        group_id,
        original_name
    )
    .fetch_optional(executor)
    .await?;

    Ok(result)
}

pub async fn admin_update_group_metadata<'e, E>(
    executor: E,
    group_id: i32,
    size: i64,
    sha256: &str,
) -> Result<Option<RbAssetGroupAdminData>, RbInternalError>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query_as!(
        RbAssetGroupAdminData,
        "UPDATE rb_asset_group
        SET size = $2, sha256 = $3
        WHERE id = $1
        RETURNING id, game_id, puzzle_id, round_id, backend, object_key, original_name, mime_type, size, sha256, ctime_at;",
        group_id,
        size,
        sha256
    )
    .fetch_optional(executor)
    .await?;

    Ok(result)
}

pub async fn admin_update_file_path<'e, E>(
    executor: E,
    group_id: i32,
    file_id: i32,
    relative_path: &str,
) -> Result<Option<RbAssetFileAdminData>, RbInternalError>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query_as!(
        RbAssetFileAdminData,
        "UPDATE rb_asset_file
        SET relative_path = $3
        WHERE group_id = $1 AND id = $2
        RETURNING id, group_id, relative_path, mime_type, size, sha256, ctime_at;",
        group_id,
        file_id,
        relative_path
    )
    .fetch_optional(executor)
    .await?;

    Ok(result)
}

pub async fn admin_delete_file<'e, E>(
    executor: E,
    group_id: i32,
    file_id: i32,
) -> Result<bool, RbInternalError>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        "DELETE FROM rb_asset_file
        WHERE group_id = $1 AND id = $2;",
        group_id,
        file_id
    )
    .execute(executor)
    .await?;

    Ok(result.rows_affected() > 0)
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
