use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use time::OffsetDateTime;

use crate::{DbPool, error::RbInternalError};

#[derive(Clone, FromRow, Serialize)]
pub struct PuzzleBackend {
    pub puzzle_id: i32,
    pub enabled: bool,
    pub source: String,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub ctime_at: OffsetDateTime,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub utime_at: OffsetDateTime,
}

#[derive(Clone, Deserialize)]
pub struct PuzzleBackendInput {
    #[serde(default)]
    pub enabled: Option<bool>,
    pub source: String,
}

#[derive(Clone, FromRow, Serialize)]
pub struct PuzzleBackendKvEntry {
    pub key: String,
    pub value: Value,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub utime_at: OffsetDateTime,
}

#[derive(Clone, Serialize)]
pub struct PuzzleStoreDocUser {
    pub id: i32,
    pub nickname: String,
}

#[derive(Clone, Serialize)]
pub struct PuzzleStoreDocTeam {
    pub id: i32,
    pub name: String,
}

#[derive(Clone, Serialize)]
pub struct PuzzleStoreDoc {
    pub id: i64,
    pub collection: String,
    pub team: Option<PuzzleStoreDocTeam>,
    pub user: Option<PuzzleStoreDocUser>,
    pub value: Value,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub ctime_at: OffsetDateTime,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub utime_at: OffsetDateTime,
}

#[derive(Clone)]
pub enum PuzzleStoreIndexValue {
    Text(String),
    Number(f64),
    Bool(bool),
}

#[derive(Clone)]
pub struct PuzzleStoreIndexEntry {
    pub key: String,
    pub value: PuzzleStoreIndexValue,
}

#[derive(Clone)]
pub struct PuzzleStoreEqFilters {
    pub text: Vec<(String, String)>,
    pub number: Vec<(String, f64)>,
    pub bool_: Vec<(String, bool)>,
}

impl PuzzleStoreEqFilters {
    pub fn empty() -> Self {
        Self {
            text: vec![],
            number: vec![],
            bool_: vec![],
        }
    }

    fn len(&self) -> i64 {
        (self.text.len() + self.number.len() + self.bool_.len()) as i64
    }
}

#[derive(Clone)]
pub struct PuzzleStoreListOptions {
    pub filters: PuzzleStoreEqFilters,
    pub cursor: Option<i64>,
    pub limit: i64,
    pub descending: bool,
}

struct StoreDocRow {
    id: i64,
    collection: String,
    team_id: Option<i32>,
    team_name: Option<String>,
    user_id: Option<i32>,
    user_nickname: Option<String>,
    value: Value,
    ctime_at: OffsetDateTime,
    utime_at: OffsetDateTime,
}

fn store_doc_from_row(row: StoreDocRow) -> PuzzleStoreDoc {
    PuzzleStoreDoc {
        id: row.id,
        collection: row.collection,
        team: row
            .team_id
            .zip(row.team_name)
            .map(|(id, name)| PuzzleStoreDocTeam { id, name }),
        user: row
            .user_id
            .zip(row.user_nickname)
            .map(|(id, nickname)| PuzzleStoreDocUser { id, nickname }),
        value: row.value,
        ctime_at: row.ctime_at,
        utime_at: row.utime_at,
    }
}

pub async fn get_backend(
    db_pool: &DbPool,
    puzzle_id: i32,
) -> Result<Option<PuzzleBackend>, RbInternalError> {
    let row = sqlx::query_as!(
        PuzzleBackend,
        r#"SELECT puzzle_id, enabled, source, ctime_at, utime_at
        FROM rb_puzzle_backend
        WHERE puzzle_id = $1"#,
        puzzle_id
    )
    .fetch_optional(db_pool)
    .await?;

    Ok(row)
}

pub async fn upsert_backend(
    db_pool: &DbPool,
    puzzle_id: i32,
    data: &PuzzleBackendInput,
) -> Result<PuzzleBackend, RbInternalError> {
    let enabled = data.enabled.unwrap_or(false);

    let row = sqlx::query_as!(
        PuzzleBackend,
        r#"INSERT INTO rb_puzzle_backend
            (puzzle_id, enabled, source)
        VALUES ($1, $2, $3)
        ON CONFLICT (puzzle_id)
        DO UPDATE SET
            enabled = EXCLUDED.enabled,
            source = EXCLUDED.source,
            utime_at = CURRENT_TIMESTAMP
        RETURNING puzzle_id, enabled, source, ctime_at, utime_at"#,
        puzzle_id,
        enabled,
        &data.source
    )
    .fetch_one(db_pool)
    .await?;

    Ok(row)
}

pub async fn update_backend_source(
    db_pool: &DbPool,
    puzzle_id: i32,
    source: &str,
) -> Result<PuzzleBackend, RbInternalError> {
    let row = sqlx::query_as!(
        PuzzleBackend,
        r#"INSERT INTO rb_puzzle_backend (puzzle_id, enabled, source)
        VALUES ($1, FALSE, $2)
        ON CONFLICT (puzzle_id)
        DO UPDATE SET
            source = EXCLUDED.source,
            utime_at = CURRENT_TIMESTAMP
        RETURNING puzzle_id, enabled, source, ctime_at, utime_at"#,
        puzzle_id,
        source
    )
    .fetch_one(db_pool)
    .await?;

    Ok(row)
}

pub async fn delete_backend(db_pool: &DbPool, puzzle_id: i32) -> Result<bool, RbInternalError> {
    let result = sqlx::query!(
        r#"DELETE FROM rb_puzzle_backend
        WHERE puzzle_id = $1"#,
        puzzle_id
    )
    .execute(db_pool)
    .await?;

    Ok(result.rows_affected() > 0)
}

pub async fn get_kv(
    db_pool: &DbPool,
    puzzle_id: i32,
    team_id: Option<i32>,
    key: &str,
) -> Result<Option<Value>, RbInternalError> {
    let row = sqlx::query!(
        r#"SELECT value FROM rb_puzzle_kv
        WHERE puzzle_id = $1 AND key = $2 AND team_id IS NOT DISTINCT FROM $3"#,
        puzzle_id,
        key,
        team_id
    )
    .fetch_optional(db_pool)
    .await?;

    Ok(row.map(|row| row.value))
}

pub async fn set_kv(
    db_pool: &DbPool,
    puzzle_id: i32,
    team_id: Option<i32>,
    key: &str,
    value: &Value,
) -> Result<Value, RbInternalError> {
    let value = if team_id.is_some() {
        sqlx::query_scalar!(
            r#"INSERT INTO rb_puzzle_kv (puzzle_id, team_id, key, value)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (puzzle_id, team_id, key) WHERE team_id IS NOT NULL
            DO UPDATE SET value = EXCLUDED.value, utime_at = CURRENT_TIMESTAMP
            RETURNING value"#,
            puzzle_id,
            team_id,
            key,
            value
        )
        .fetch_one(db_pool)
        .await?
    } else {
        sqlx::query_scalar!(
            r#"INSERT INTO rb_puzzle_kv (puzzle_id, team_id, key, value)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (puzzle_id, key) WHERE team_id IS NULL
            DO UPDATE SET value = EXCLUDED.value, utime_at = CURRENT_TIMESTAMP
            RETURNING value"#,
            puzzle_id,
            team_id,
            key,
            value
        )
        .fetch_one(db_pool)
        .await?
    };

    Ok(value)
}

pub async fn delete_kv(
    db_pool: &DbPool,
    puzzle_id: i32,
    team_id: Option<i32>,
    key: &str,
) -> Result<bool, RbInternalError> {
    let result = sqlx::query!(
        r#"DELETE FROM rb_puzzle_kv
        WHERE puzzle_id = $1 AND key = $2 AND team_id IS NOT DISTINCT FROM $3"#,
        puzzle_id,
        key,
        team_id
    )
    .execute(db_pool)
    .await?;

    Ok(result.rows_affected() > 0)
}

pub async fn list_kv(
    db_pool: &DbPool,
    puzzle_id: i32,
    team_id: Option<i32>,
    prefix: Option<&str>,
) -> Result<Vec<PuzzleBackendKvEntry>, RbInternalError> {
    let pattern = prefix.map(|value| format!("{value}%"));
    let rows = if let Some(pattern) = pattern.as_deref() {
        sqlx::query_as!(
            PuzzleBackendKvEntry,
            r#"SELECT key, value, utime_at
            FROM rb_puzzle_kv
            WHERE puzzle_id = $1
                AND team_id IS NOT DISTINCT FROM $2
                AND key LIKE $3
            ORDER BY key"#,
            puzzle_id,
            team_id,
            pattern
        )
        .fetch_all(db_pool)
        .await?
    } else {
        sqlx::query_as!(
            PuzzleBackendKvEntry,
            r#"SELECT key, value, utime_at
            FROM rb_puzzle_kv
            WHERE puzzle_id = $1
                AND team_id IS NOT DISTINCT FROM $2
            ORDER BY key"#,
            puzzle_id,
            team_id
        )
        .fetch_all(db_pool)
        .await?
    };

    Ok(rows)
}

pub async fn clear_puzzle_team_kv(
    db_pool: &DbPool,
    puzzle_id: i32,
) -> Result<u64, RbInternalError> {
    let result = sqlx::query!(
        r#"DELETE FROM rb_puzzle_kv
        WHERE puzzle_id = $1 AND team_id IS NOT NULL"#,
        puzzle_id
    )
    .execute(db_pool)
    .await?;

    Ok(result.rows_affected())
}

pub async fn insert_store_doc(
    db_pool: &DbPool,
    puzzle_id: i32,
    collection: &str,
    team_id: Option<i32>,
    user_id: Option<i32>,
    value: &Value,
    indexes: &[PuzzleStoreIndexEntry],
) -> Result<PuzzleStoreDoc, RbInternalError> {
    let mut tx = db_pool.begin().await?;

    let row = sqlx::query!(
        r#"INSERT INTO rb_puzzle_store_doc
            (puzzle_id, collection, team_id, user_id, value)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id, collection, team_id, user_id, value, ctime_at, utime_at"#,
        puzzle_id,
        collection,
        team_id,
        user_id,
        value
    )
    .fetch_one(&mut *tx)
    .await?;

    for index in indexes {
        match &index.value {
            PuzzleStoreIndexValue::Text(value) => {
                sqlx::query!(
                    r#"INSERT INTO rb_puzzle_store_index
                        (doc_id, puzzle_id, collection, key, value_text)
                    VALUES ($1, $2, $3, $4, $5)"#,
                    row.id,
                    puzzle_id,
                    collection,
                    &index.key,
                    value
                )
                .execute(&mut *tx)
                .await?;
            }
            PuzzleStoreIndexValue::Number(value) => {
                sqlx::query!(
                    r#"INSERT INTO rb_puzzle_store_index
                        (doc_id, puzzle_id, collection, key, value_number)
                    VALUES ($1, $2, $3, $4, $5)"#,
                    row.id,
                    puzzle_id,
                    collection,
                    &index.key,
                    value
                )
                .execute(&mut *tx)
                .await?;
            }
            PuzzleStoreIndexValue::Bool(value) => {
                sqlx::query!(
                    r#"INSERT INTO rb_puzzle_store_index
                        (doc_id, puzzle_id, collection, key, value_bool)
                    VALUES ($1, $2, $3, $4, $5)"#,
                    row.id,
                    puzzle_id,
                    collection,
                    &index.key,
                    value
                )
                .execute(&mut *tx)
                .await?;
            }
        }
    }

    let team = if let Some(team_id) = team_id {
        sqlx::query!("SELECT name FROM rb_team WHERE id = $1", team_id)
            .fetch_optional(&mut *tx)
            .await?
            .map(|row| row.name)
    } else {
        None
    };
    let user = if let Some(user_id) = user_id {
        sqlx::query!("SELECT nickname FROM rb_user WHERE id = $1", user_id)
            .fetch_optional(&mut *tx)
            .await?
            .map(|row| row.nickname)
    } else {
        None
    };

    tx.commit().await?;

    Ok(store_doc_from_row(StoreDocRow {
        id: row.id,
        collection: row.collection,
        team_id: row.team_id,
        team_name: team,
        user_id: row.user_id,
        user_nickname: user,
        value: row.value,
        ctime_at: row.ctime_at,
        utime_at: row.utime_at,
    }))
}

pub async fn get_store_doc(
    db_pool: &DbPool,
    puzzle_id: i32,
    collection: &str,
    doc_id: i64,
) -> Result<Option<PuzzleStoreDoc>, RbInternalError> {
    let row = sqlx::query!(
        r#"SELECT d.id, d.collection, d.team_id, t.name AS team_name,
            d.user_id, u.nickname AS user_nickname, d.value, d.ctime_at, d.utime_at
        FROM rb_puzzle_store_doc d
        LEFT JOIN rb_team t ON t.id = d.team_id
        LEFT JOIN rb_user u ON u.id = d.user_id
        WHERE d.puzzle_id = $1 AND d.collection = $2 AND d.id = $3"#,
        puzzle_id,
        collection,
        doc_id
    )
    .fetch_optional(db_pool)
    .await?;

    Ok(row.map(|row| {
        store_doc_from_row(StoreDocRow {
            id: row.id,
            collection: row.collection,
            team_id: row.team_id,
            team_name: Some(row.team_name),
            user_id: row.user_id,
            user_nickname: Some(row.user_nickname),
            value: row.value,
            ctime_at: row.ctime_at,
            utime_at: row.utime_at,
        })
    }))
}

pub async fn list_store_docs(
    db_pool: &DbPool,
    puzzle_id: i32,
    collection: &str,
    options: &PuzzleStoreListOptions,
) -> Result<Vec<PuzzleStoreDoc>, RbInternalError> {
    let text_keys: Vec<String> = options
        .filters
        .text
        .iter()
        .map(|(key, _)| key.clone())
        .collect();
    let text_values: Vec<String> = options
        .filters
        .text
        .iter()
        .map(|(_, value)| value.clone())
        .collect();
    let number_keys: Vec<String> = options
        .filters
        .number
        .iter()
        .map(|(key, _)| key.clone())
        .collect();
    let number_values: Vec<f64> = options
        .filters
        .number
        .iter()
        .map(|(_, value)| *value)
        .collect();
    let bool_keys: Vec<String> = options
        .filters
        .bool_
        .iter()
        .map(|(key, _)| key.clone())
        .collect();
    let bool_values: Vec<bool> = options
        .filters
        .bool_
        .iter()
        .map(|(_, value)| *value)
        .collect();
    let filter_count = options.filters.len();
    let limit = options.limit.clamp(1, 100);

    let rows = sqlx::query!(
        r#"SELECT d.id, d.collection, d.team_id, t.name AS "team_name?",
            d.user_id, u.nickname AS "user_nickname?", d.value, d.ctime_at, d.utime_at
        FROM rb_puzzle_store_doc d
        LEFT JOIN rb_team t ON t.id = d.team_id
        LEFT JOIN rb_user u ON u.id = d.user_id
        WHERE d.puzzle_id = $1
            AND d.collection = $2
            AND (
                $3::BIGINT IS NULL
                OR ($12::BOOLEAN AND d.id < $3)
                OR (NOT $12::BOOLEAN AND d.id > $3)
            )
            AND (
                $4::BIGINT = 0
                OR (
                    SELECT COUNT(DISTINCT i.key)
                    FROM rb_puzzle_store_index i
                    WHERE i.doc_id = d.id
                        AND i.puzzle_id = d.puzzle_id
                        AND i.collection = d.collection
                        AND (
                            (i.key, i.value_text) IN (
                                SELECT * FROM UNNEST($5::TEXT[], $6::TEXT[])
                            )
                            OR (i.key, i.value_number) IN (
                                SELECT * FROM UNNEST($7::TEXT[], $8::DOUBLE PRECISION[])
                            )
                            OR (i.key, i.value_bool) IN (
                                SELECT * FROM UNNEST($9::TEXT[], $10::BOOLEAN[])
                            )
                        )
                ) = $4
            )
        ORDER BY
            CASE WHEN $12::BOOLEAN THEN d.id END DESC,
            CASE WHEN NOT $12::BOOLEAN THEN d.id END ASC
        LIMIT $11"#,
        puzzle_id,
        collection,
        options.cursor,
        filter_count,
        &text_keys,
        &text_values,
        &number_keys,
        &number_values,
        &bool_keys,
        &bool_values,
        limit,
        options.descending
    )
    .fetch_all(db_pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| {
            store_doc_from_row(StoreDocRow {
                id: row.id,
                collection: row.collection,
                team_id: row.team_id,
                team_name: row.team_name,
                user_id: row.user_id,
                user_nickname: row.user_nickname,
                value: row.value,
                ctime_at: row.ctime_at,
                utime_at: row.utime_at,
            })
        })
        .collect())
}

pub async fn log_call(
    db_pool: &DbPool,
    puzzle_id: i32,
    team_id: Option<i32>,
    user_id: i32,
    function_name: &str,
    ok: bool,
    error: Option<&str>,
) -> Result<(), RbInternalError> {
    sqlx::query!(
        r#"INSERT INTO rb_puzzle_backend_call_log
            (puzzle_id, team_id, user_id, function_name, ok, error)
        VALUES ($1, $2, $3, $4, $5, $6)"#,
        puzzle_id,
        team_id,
        user_id,
        function_name,
        ok,
        error
    )
    .execute(db_pool)
    .await?;

    Ok(())
}
