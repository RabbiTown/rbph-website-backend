use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, PgConnection};
use time::OffsetDateTime;

use crate::{DbPool, error::RbInternalError};

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BackendScope {
    Global,
    Team { team_id: i32 },
    Puzzle { puzzle_id: i32 },
    TeamPuzzle { team_id: i32, puzzle_id: i32 },
}

impl BackendScope {
    pub fn parts(&self) -> (i16, Option<i32>, Option<i32>) {
        match *self {
            Self::Global => (0, None, None),
            Self::Team { team_id } => (1, Some(team_id), None),
            Self::Puzzle { puzzle_id } => (2, None, Some(puzzle_id)),
            Self::TeamPuzzle { team_id, puzzle_id } => (3, Some(team_id), Some(puzzle_id)),
        }
    }
}

#[derive(Clone, FromRow, Serialize)]
pub struct PuzzleBackend {
    pub puzzle_id: i32,
    pub enabled: bool,
    pub source: String,
    pub functions: Value,
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
    #[serde(default)]
    pub functions: Vec<String>,
}

static EXPORT_FUNCTION_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^export\s+(?:async\s+)?function\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(")
        .expect("valid export function regex")
});

static EXPORT_CONST_FUNCTION_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^export\s+const\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(?:async\s*)?(?:function|\()")
        .expect("valid export const function regex")
});

pub fn is_valid_backend_function_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.len() <= 64
}

fn normalize_functions(value: &Value) -> Vec<String> {
    let Some(items) = value.as_array() else {
        return vec![];
    };

    let mut result = Vec::with_capacity(items.len());
    for item in items {
        let Some(name) = item.as_str() else {
            continue;
        };
        if !result.iter().any(|existing| existing == name) {
            result.push(name.to_string());
        }
    }
    result
}

pub fn parse_export_functions(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    for pattern in [&*EXPORT_FUNCTION_PATTERN, &*EXPORT_CONST_FUNCTION_PATTERN] {
        for capture in pattern.captures_iter(source) {
            let Some(name) = capture.get(1).map(|m| m.as_str()) else {
                continue;
            };
            if is_valid_backend_function_name(name)
                && !names.iter().any(|existing| existing == name)
            {
                names.push(name.to_string());
            }
        }
    }
    names.sort();
    names
}

impl PuzzleBackend {
    pub fn functions_list(&self) -> Vec<String> {
        normalize_functions(&self.functions)
    }

    pub fn function_enabled(&self, name: &str) -> bool {
        self.functions_list().iter().any(|value| value == name)
    }

    pub fn export_enabled(&self, name: &str) -> bool {
        parse_export_functions(&self.source)
            .iter()
            .any(|value| value == name)
    }

    pub fn callable_function(&self, name: &str) -> bool {
        self.function_enabled(name) && self.export_enabled(name)
    }
}

#[derive(Clone, FromRow, Serialize)]
pub struct PuzzleBackendKvEntry {
    pub scope_type: i16,
    pub team_id: Option<i32>,
    pub puzzle_id: Option<i32>,
    pub key: String,
    pub value: Value,
    pub version: i64,
    #[serde(with = "crate::serde_helpers::serialize_option_offset_datetime")]
    pub expires_at: Option<OffsetDateTime>,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub utime_at: OffsetDateTime,
}

#[derive(Clone, FromRow)]
pub struct PuzzleBackendKvValue {
    pub value: Value,
    pub version: i64,
    pub expires_at: Option<OffsetDateTime>,
}

pub struct PuzzleBackendKvMutation {
    pub applied: bool,
    pub entry: Option<PuzzleBackendKvValue>,
    pub server_time: OffsetDateTime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PuzzleBackendKvExpiry {
    Preserve,
    Permanent,
    Ttl(i64),
}

#[derive(Clone, Serialize)]
pub struct PuzzleStoreDocUser {
    pub id: i32,
    pub nickname: String,
}

#[derive(Clone, Serialize)]
pub struct PuzzleStoreDoc {
    pub id: i64,
    pub scope: BackendScope,
    pub collection: String,
    pub created_by: Option<PuzzleStoreDocUser>,
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

#[derive(Clone, Serialize)]
pub struct PuzzleBackendCallLog {
    pub id: i64,
    pub puzzle_id: i32,
    pub team_id: Option<i32>,
    pub team_name: Option<String>,
    pub user_id: Option<i32>,
    pub user_nickname: Option<String>,
    pub execution_type: String,
    pub request_method: Option<String>,
    pub function_name: String,
    pub ok: bool,
    pub duration_ms: i64,
    pub submission_id: Option<i32>,
    pub hint_id: Option<i32>,
    pub error: Option<String>,
    pub console: Value,
    pub console_truncated: bool,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub ctime_at: OffsetDateTime,
}

pub struct PuzzleBackendCallLogInput<'a> {
    pub puzzle_id: i32,
    pub team_id: Option<i32>,
    pub user_id: i32,
    pub execution_type: &'a str,
    pub request_method: Option<&'a str>,
    pub function_name: &'a str,
    pub ok: bool,
    pub duration_ms: i64,
    pub submission_id: Option<i32>,
    pub hint_id: Option<i32>,
    pub error: Option<&'a str>,
    pub console: &'a Value,
    pub console_truncated: bool,
}

pub struct PuzzleBackendCallLogQuery<'a> {
    pub puzzle_id: i32,
    pub execution_type: Option<&'a str>,
    pub function_name: Option<&'a str>,
    pub ok: Option<bool>,
    pub team_id: Option<i32>,
    pub user_id: Option<i32>,
    pub from: Option<OffsetDateTime>,
    pub to: Option<OffsetDateTime>,
    pub offset: i64,
    pub limit: i64,
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

pub struct ClearPuzzleTeamBackendStateResult {
    pub rows: usize,
    pub team_ids: Vec<i32>,
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
    scope_type: i16,
    team_id: Option<i32>,
    puzzle_id: Option<i32>,
    collection: String,
    created_by: Option<i32>,
    created_by_nickname: Option<String>,
    value: Value,
    ctime_at: OffsetDateTime,
    utime_at: OffsetDateTime,
}

fn store_doc_from_row(row: StoreDocRow) -> PuzzleStoreDoc {
    PuzzleStoreDoc {
        id: row.id,
        scope: match row.scope_type {
            1 => BackendScope::Team {
                team_id: row.team_id.unwrap_or_default(),
            },
            2 => BackendScope::Puzzle {
                puzzle_id: row.puzzle_id.unwrap_or_default(),
            },
            3 => BackendScope::TeamPuzzle {
                team_id: row.team_id.unwrap_or_default(),
                puzzle_id: row.puzzle_id.unwrap_or_default(),
            },
            _ => BackendScope::Global,
        },
        collection: row.collection,
        created_by: row
            .created_by
            .zip(row.created_by_nickname)
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
        r#"SELECT puzzle_id, enabled, source, functions, ctime_at, utime_at
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
            (puzzle_id, enabled, source, functions)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (puzzle_id)
        DO UPDATE SET
            enabled = EXCLUDED.enabled,
            source = EXCLUDED.source,
            functions = EXCLUDED.functions,
            utime_at = CURRENT_TIMESTAMP
        RETURNING puzzle_id, enabled, source, functions, ctime_at, utime_at"#,
        puzzle_id,
        enabled,
        &data.source,
        serde_json::Value::Array(
            data.functions
                .iter()
                .cloned()
                .map(serde_json::Value::String)
                .collect(),
        )
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
        r#"INSERT INTO rb_puzzle_backend (puzzle_id, enabled, source, functions)
        VALUES ($1, FALSE, $2, '[]'::JSONB)
        ON CONFLICT (puzzle_id)
        DO UPDATE SET
            source = EXCLUDED.source,
            utime_at = CURRENT_TIMESTAMP
        RETURNING puzzle_id, enabled, source, functions, ctime_at, utime_at"#,
        puzzle_id,
        source
    )
    .fetch_one(db_pool)
    .await?;

    Ok(row)
}

pub async fn update_backend_functions(
    db_pool: &DbPool,
    puzzle_id: i32,
    functions: &[String],
) -> Result<PuzzleBackend, RbInternalError> {
    let functions = serde_json::Value::Array(
        functions
            .iter()
            .cloned()
            .map(serde_json::Value::String)
            .collect(),
    );
    let row = sqlx::query_as!(
        PuzzleBackend,
        r#"UPDATE rb_puzzle_backend
        SET functions = $2, utime_at = CURRENT_TIMESTAMP
        WHERE puzzle_id = $1
        RETURNING puzzle_id, enabled, source, functions, ctime_at, utime_at"#,
        puzzle_id,
        functions
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

pub async fn ensure_scope_in_game(
    db_pool: &DbPool,
    game_id: i32,
    scope: BackendScope,
) -> Result<bool, RbInternalError> {
    let mut conn = db_pool.acquire().await?;
    ensure_scope_in_game_conn(&mut conn, game_id, scope).await
}

pub async fn ensure_scope_in_game_conn(
    conn: &mut PgConnection,
    game_id: i32,
    scope: BackendScope,
) -> Result<bool, RbInternalError> {
    let valid = match scope {
        BackendScope::Global => true,
        BackendScope::Team { team_id } => sqlx::query_scalar!(
            "SELECT EXISTS (SELECT 1 FROM rb_team WHERE id = $1 AND game_id = $2) AS \"exists!\"",
            team_id,
            game_id
        )
        .fetch_one(&mut *conn)
        .await?,
        BackendScope::Puzzle { puzzle_id } => sqlx::query_scalar!(
            "SELECT EXISTS (SELECT 1 FROM rb_puzzle WHERE id = $1 AND game_id = $2) AS \"exists!\"",
            puzzle_id,
            game_id
        )
        .fetch_one(&mut *conn)
        .await?,
        BackendScope::TeamPuzzle { team_id, puzzle_id } => {
            sqlx::query_scalar!(
                "SELECT EXISTS (
                    SELECT 1 FROM rb_team t
                    JOIN rb_puzzle p ON p.game_id = t.game_id
                    WHERE t.id = $1 AND p.id = $2 AND t.game_id = $3
                ) AS \"exists!\"",
                team_id,
                puzzle_id,
                game_id
            )
            .fetch_one(&mut *conn)
            .await?
        }
    };

    Ok(valid)
}

async fn require_scope_in_game(
    db_pool: &DbPool,
    game_id: i32,
    scope: BackendScope,
) -> Result<(), RbInternalError> {
    if ensure_scope_in_game(db_pool, game_id, scope).await? {
        Ok(())
    } else {
        Err(RbInternalError::Other(
            "backend scope does not belong to current game".to_string(),
        ))
    }
}

async fn require_scope_in_game_conn(
    conn: &mut PgConnection,
    game_id: i32,
    scope: BackendScope,
) -> Result<(), RbInternalError> {
    if ensure_scope_in_game_conn(conn, game_id, scope).await? {
        Ok(())
    } else {
        Err(RbInternalError::Other(
            "backend scope does not belong to current game".to_string(),
        ))
    }
}

pub async fn get_kv(
    db_pool: &DbPool,
    game_id: i32,
    scope: BackendScope,
    key: &str,
) -> Result<Option<Value>, RbInternalError> {
    let mut conn = db_pool.acquire().await?;
    get_kv_conn(&mut conn, game_id, scope, key).await
}

pub async fn get_kv_conn(
    conn: &mut PgConnection,
    game_id: i32,
    scope: BackendScope,
    key: &str,
) -> Result<Option<Value>, RbInternalError> {
    Ok(get_kv_entry_conn(conn, game_id, scope, key)
        .await?
        .map(|entry| entry.value))
}

pub async fn get_kv_entry(
    db_pool: &DbPool,
    game_id: i32,
    scope: BackendScope,
    key: &str,
) -> Result<Option<PuzzleBackendKvValue>, RbInternalError> {
    let mut conn = db_pool.acquire().await?;
    get_kv_entry_conn(&mut conn, game_id, scope, key).await
}

pub async fn get_kv_entry_conn(
    conn: &mut PgConnection,
    game_id: i32,
    scope: BackendScope,
    key: &str,
) -> Result<Option<PuzzleBackendKvValue>, RbInternalError> {
    require_scope_in_game_conn(conn, game_id, scope).await?;
    let (scope_type, team_id, puzzle_id) = scope.parts();
    let row = sqlx::query_as!(
        PuzzleBackendKvValue,
        r#"SELECT value, version, expires_at FROM rb_puzzle_kv
        WHERE game_id = $1 AND scope_type = $2
            AND team_id IS NOT DISTINCT FROM $3
            AND puzzle_id IS NOT DISTINCT FROM $4
            AND key = $5
            AND (expires_at IS NULL OR expires_at > statement_timestamp())"#,
        game_id,
        scope_type,
        team_id,
        puzzle_id,
        key
    )
    .fetch_optional(&mut *conn)
    .await?;

    Ok(row)
}

pub async fn set_kv(
    db_pool: &DbPool,
    game_id: i32,
    scope: BackendScope,
    key: &str,
    value: &Value,
) -> Result<Value, RbInternalError> {
    let mut conn = db_pool.acquire().await?;
    set_kv_conn(&mut conn, game_id, scope, key, value).await
}

pub async fn set_kv_conn(
    conn: &mut PgConnection,
    game_id: i32,
    scope: BackendScope,
    key: &str,
    value: &Value,
) -> Result<Value, RbInternalError> {
    require_scope_in_game_conn(conn, game_id, scope).await?;
    let (scope_type, team_id, puzzle_id) = scope.parts();
    let value = sqlx::query_scalar!(
        r#"INSERT INTO rb_puzzle_kv (game_id, scope_type, team_id, puzzle_id, key, value)
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (game_id, scope_type, team_id, puzzle_id, key)
        DO UPDATE SET value = EXCLUDED.value,
            version = rb_puzzle_kv.version + 1,
            expires_at = NULL,
            utime_at = CURRENT_TIMESTAMP
        RETURNING value"#,
        game_id,
        scope_type,
        team_id,
        puzzle_id,
        key,
        value
    )
    .fetch_one(&mut *conn)
    .await?;

    Ok(value)
}

async fn current_kv_mutation_conn(
    conn: &mut PgConnection,
    game_id: i32,
    scope: BackendScope,
    key: &str,
    applied_entry: Option<PuzzleBackendKvValue>,
) -> Result<PuzzleBackendKvMutation, RbInternalError> {
    let applied = applied_entry.is_some();
    let entry = if applied {
        applied_entry
    } else {
        get_kv_entry_conn(conn, game_id, scope, key).await?
    };
    let server_time =
        sqlx::query_scalar!(r#"SELECT statement_timestamp() AS "server_time!: OffsetDateTime""#)
            .fetch_one(&mut *conn)
            .await?;
    Ok(PuzzleBackendKvMutation {
        applied,
        entry,
        server_time,
    })
}

pub async fn set_kv_if_absent(
    db_pool: &DbPool,
    game_id: i32,
    scope: BackendScope,
    key: &str,
    value: &Value,
    ttl_ms: Option<i64>,
) -> Result<PuzzleBackendKvMutation, RbInternalError> {
    let mut conn = db_pool.acquire().await?;
    set_kv_if_absent_conn(&mut conn, game_id, scope, key, value, ttl_ms).await
}

pub async fn set_kv_if_absent_conn(
    conn: &mut PgConnection,
    game_id: i32,
    scope: BackendScope,
    key: &str,
    value: &Value,
    ttl_ms: Option<i64>,
) -> Result<PuzzleBackendKvMutation, RbInternalError> {
    require_scope_in_game_conn(conn, game_id, scope).await?;
    let (scope_type, team_id, puzzle_id) = scope.parts();
    let entry = sqlx::query_as!(
        PuzzleBackendKvValue,
        r#"INSERT INTO rb_puzzle_kv (
            game_id, scope_type, team_id, puzzle_id, key, value, expires_at
        )
        VALUES (
            $1, $2, $3, $4, $5, $6,
            CASE WHEN $7::BIGINT IS NULL THEN NULL
                ELSE statement_timestamp() + $7 * INTERVAL '1 millisecond' END
        )
        ON CONFLICT (game_id, scope_type, team_id, puzzle_id, key)
        DO UPDATE SET value = EXCLUDED.value,
            version = rb_puzzle_kv.version + 1,
            expires_at = EXCLUDED.expires_at,
            utime_at = CURRENT_TIMESTAMP
        WHERE rb_puzzle_kv.expires_at IS NOT NULL
            AND rb_puzzle_kv.expires_at <= statement_timestamp()
        RETURNING value, version, expires_at"#,
        game_id,
        scope_type,
        team_id,
        puzzle_id,
        key,
        value,
        ttl_ms
    )
    .fetch_optional(&mut *conn)
    .await?;

    current_kv_mutation_conn(conn, game_id, scope, key, entry).await
}

pub async fn compare_and_set_kv(
    db_pool: &DbPool,
    game_id: i32,
    scope: BackendScope,
    key: &str,
    expected_version: i64,
    value: &Value,
    expiry: PuzzleBackendKvExpiry,
) -> Result<PuzzleBackendKvMutation, RbInternalError> {
    let mut conn = db_pool.acquire().await?;
    compare_and_set_kv_conn(
        &mut conn,
        game_id,
        scope,
        key,
        expected_version,
        value,
        expiry,
    )
    .await
}

pub async fn compare_and_set_kv_conn(
    conn: &mut PgConnection,
    game_id: i32,
    scope: BackendScope,
    key: &str,
    expected_version: i64,
    value: &Value,
    expiry: PuzzleBackendKvExpiry,
) -> Result<PuzzleBackendKvMutation, RbInternalError> {
    require_scope_in_game_conn(conn, game_id, scope).await?;
    let (scope_type, team_id, puzzle_id) = scope.parts();
    let (preserve_expiry, ttl_ms) = match expiry {
        PuzzleBackendKvExpiry::Preserve => (true, None),
        PuzzleBackendKvExpiry::Permanent => (false, None),
        PuzzleBackendKvExpiry::Ttl(ttl_ms) => (false, Some(ttl_ms)),
    };
    let entry = sqlx::query_as!(
        PuzzleBackendKvValue,
        r#"UPDATE rb_puzzle_kv SET
            value = $6,
            version = version + 1,
            expires_at = CASE
                WHEN $7 THEN expires_at
                WHEN $8::BIGINT IS NULL THEN NULL
                ELSE statement_timestamp() + $8 * INTERVAL '1 millisecond'
            END,
            utime_at = CURRENT_TIMESTAMP
        WHERE game_id = $1 AND scope_type = $2
            AND team_id IS NOT DISTINCT FROM $3
            AND puzzle_id IS NOT DISTINCT FROM $4
            AND key = $5
            AND version = $9
            AND (expires_at IS NULL OR expires_at > statement_timestamp())
        RETURNING value, version, expires_at"#,
        game_id,
        scope_type,
        team_id,
        puzzle_id,
        key,
        value,
        preserve_expiry,
        ttl_ms,
        expected_version
    )
    .fetch_optional(&mut *conn)
    .await?;

    current_kv_mutation_conn(conn, game_id, scope, key, entry).await
}

pub async fn delete_kv(
    db_pool: &DbPool,
    game_id: i32,
    scope: BackendScope,
    key: &str,
) -> Result<bool, RbInternalError> {
    let mut conn = db_pool.acquire().await?;
    delete_kv_conn(&mut conn, game_id, scope, key).await
}

pub async fn delete_kv_conn(
    conn: &mut PgConnection,
    game_id: i32,
    scope: BackendScope,
    key: &str,
) -> Result<bool, RbInternalError> {
    require_scope_in_game_conn(conn, game_id, scope).await?;
    let (scope_type, team_id, puzzle_id) = scope.parts();
    let result = sqlx::query!(
        r#"DELETE FROM rb_puzzle_kv
        WHERE game_id = $1 AND scope_type = $2
            AND team_id IS NOT DISTINCT FROM $3
            AND puzzle_id IS NOT DISTINCT FROM $4
            AND key = $5"#,
        game_id,
        scope_type,
        team_id,
        puzzle_id,
        key
    )
    .execute(&mut *conn)
    .await?;

    Ok(result.rows_affected() > 0)
}

pub async fn list_kv(
    db_pool: &DbPool,
    game_id: i32,
    scope: BackendScope,
    prefix: Option<&str>,
) -> Result<Vec<PuzzleBackendKvEntry>, RbInternalError> {
    require_scope_in_game(db_pool, game_id, scope).await?;
    let (scope_type, team_id, puzzle_id) = scope.parts();
    let pattern = prefix.map(|value| format!("{value}%"));
    let rows = sqlx::query_as!(
        PuzzleBackendKvEntry,
        r#"SELECT scope_type, team_id, puzzle_id, key, value,
            version, expires_at, utime_at
        FROM rb_puzzle_kv
        WHERE game_id = $1 AND scope_type = $2
            AND team_id IS NOT DISTINCT FROM $3
            AND puzzle_id IS NOT DISTINCT FROM $4
            AND ($5::TEXT IS NULL OR key LIKE $5)
        ORDER BY key"#,
        game_id,
        scope_type,
        team_id,
        puzzle_id,
        pattern
    )
    .fetch_all(db_pool)
    .await?;

    Ok(rows)
}

pub async fn clear_puzzle_team_kv(
    db_pool: &DbPool,
    puzzle_id: i32,
) -> Result<ClearPuzzleTeamBackendStateResult, RbInternalError> {
    let team_ids = sqlx::query_scalar!(
        r#"DELETE FROM rb_puzzle_kv
        WHERE puzzle_id = $1 AND team_id IS NOT NULL
        RETURNING team_id AS "team_id!""#,
        puzzle_id
    )
    .fetch_all(db_pool)
    .await?;

    Ok(ClearPuzzleTeamBackendStateResult {
        rows: team_ids.len(),
        team_ids,
    })
}

pub async fn clear_puzzle_team_store(
    db_pool: &DbPool,
    puzzle_id: i32,
) -> Result<ClearPuzzleTeamBackendStateResult, RbInternalError> {
    let team_ids = sqlx::query_scalar!(
        r#"DELETE FROM rb_puzzle_store_doc
        WHERE puzzle_id = $1 AND team_id IS NOT NULL
        RETURNING team_id AS "team_id!""#,
        puzzle_id
    )
    .fetch_all(db_pool)
    .await?;

    Ok(ClearPuzzleTeamBackendStateResult {
        rows: team_ids.len(),
        team_ids,
    })
}

pub async fn insert_store_doc(
    db_pool: &DbPool,
    game_id: i32,
    scope: BackendScope,
    collection: &str,
    created_by: i32,
    value: &Value,
    indexes: &[PuzzleStoreIndexEntry],
) -> Result<PuzzleStoreDoc, RbInternalError> {
    let mut tx = db_pool.begin().await?;
    let doc = insert_store_doc_conn(
        &mut tx, game_id, scope, collection, created_by, value, indexes,
    )
    .await?;
    tx.commit().await?;
    Ok(doc)
}

pub async fn insert_store_doc_conn(
    conn: &mut PgConnection,
    game_id: i32,
    scope: BackendScope,
    collection: &str,
    created_by: i32,
    value: &Value,
    indexes: &[PuzzleStoreIndexEntry],
) -> Result<PuzzleStoreDoc, RbInternalError> {
    require_scope_in_game_conn(conn, game_id, scope).await?;
    let (scope_type, team_id, puzzle_id) = scope.parts();

    let row = sqlx::query!(
        r#"INSERT INTO rb_puzzle_store_doc
            (game_id, scope_type, team_id, puzzle_id, collection, created_by, value)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING id, scope_type, team_id, puzzle_id, collection, created_by, value, ctime_at, utime_at"#,
        game_id,
        scope_type,
        team_id,
        puzzle_id,
        collection,
        created_by,
        value
    )
    .fetch_one(&mut *conn)
    .await?;

    for index in indexes {
        match &index.value {
            PuzzleStoreIndexValue::Text(value) => {
                sqlx::query!(
                    r#"INSERT INTO rb_puzzle_store_index
                        (doc_id, game_id, collection, key, value_text)
                    VALUES ($1, $2, $3, $4, $5)"#,
                    row.id,
                    game_id,
                    collection,
                    &index.key,
                    value
                )
                .execute(&mut *conn)
                .await?;
            }
            PuzzleStoreIndexValue::Number(value) => {
                sqlx::query!(
                    r#"INSERT INTO rb_puzzle_store_index
                        (doc_id, game_id, collection, key, value_number)
                    VALUES ($1, $2, $3, $4, $5)"#,
                    row.id,
                    game_id,
                    collection,
                    &index.key,
                    value
                )
                .execute(&mut *conn)
                .await?;
            }
            PuzzleStoreIndexValue::Bool(value) => {
                sqlx::query!(
                    r#"INSERT INTO rb_puzzle_store_index
                        (doc_id, game_id, collection, key, value_bool)
                    VALUES ($1, $2, $3, $4, $5)"#,
                    row.id,
                    game_id,
                    collection,
                    &index.key,
                    value
                )
                .execute(&mut *conn)
                .await?;
            }
        }
    }

    let user = sqlx::query!("SELECT nickname FROM rb_user WHERE id = $1", row.created_by)
        .fetch_optional(&mut *conn)
        .await?
        .map(|row| row.nickname);

    Ok(store_doc_from_row(StoreDocRow {
        id: row.id,
        scope_type: row.scope_type,
        team_id: row.team_id,
        puzzle_id: row.puzzle_id,
        collection: row.collection,
        created_by: row.created_by,
        created_by_nickname: user,
        value: row.value,
        ctime_at: row.ctime_at,
        utime_at: row.utime_at,
    }))
}

pub async fn get_store_doc(
    db_pool: &DbPool,
    game_id: i32,
    scope: BackendScope,
    collection: &str,
    doc_id: i64,
) -> Result<Option<PuzzleStoreDoc>, RbInternalError> {
    let mut conn = db_pool.acquire().await?;
    get_store_doc_conn(&mut conn, game_id, scope, collection, doc_id).await
}

pub async fn get_store_doc_conn(
    conn: &mut PgConnection,
    game_id: i32,
    scope: BackendScope,
    collection: &str,
    doc_id: i64,
) -> Result<Option<PuzzleStoreDoc>, RbInternalError> {
    require_scope_in_game_conn(conn, game_id, scope).await?;
    let (scope_type, team_id, puzzle_id) = scope.parts();
    let row = sqlx::query!(
        r#"SELECT d.id, d.scope_type, d.team_id, d.puzzle_id, d.collection,
            d.created_by, u.nickname AS "created_by_nickname?", d.value, d.ctime_at, d.utime_at
        FROM rb_puzzle_store_doc d
        LEFT JOIN rb_user u ON u.id = d.created_by
        WHERE d.game_id = $1 AND d.scope_type = $2
            AND d.team_id IS NOT DISTINCT FROM $3
            AND d.puzzle_id IS NOT DISTINCT FROM $4
            AND d.collection = $5 AND d.id = $6"#,
        game_id,
        scope_type,
        team_id,
        puzzle_id,
        collection,
        doc_id
    )
    .fetch_optional(&mut *conn)
    .await?;

    Ok(row.map(|row| {
        store_doc_from_row(StoreDocRow {
            id: row.id,
            scope_type: row.scope_type,
            team_id: row.team_id,
            puzzle_id: row.puzzle_id,
            collection: row.collection,
            created_by: row.created_by,
            created_by_nickname: row.created_by_nickname,
            value: row.value,
            ctime_at: row.ctime_at,
            utime_at: row.utime_at,
        })
    }))
}

pub async fn list_store_docs(
    db_pool: &DbPool,
    game_id: i32,
    scope: BackendScope,
    collection: &str,
    options: &PuzzleStoreListOptions,
) -> Result<Vec<PuzzleStoreDoc>, RbInternalError> {
    let mut conn = db_pool.acquire().await?;
    list_store_docs_conn(&mut conn, game_id, scope, collection, options).await
}

pub async fn list_store_docs_conn(
    conn: &mut PgConnection,
    game_id: i32,
    scope: BackendScope,
    collection: &str,
    options: &PuzzleStoreListOptions,
) -> Result<Vec<PuzzleStoreDoc>, RbInternalError> {
    require_scope_in_game_conn(conn, game_id, scope).await?;
    let (scope_type, team_id, puzzle_id) = scope.parts();
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
        r#"SELECT d.id, d.scope_type, d.team_id, d.puzzle_id, d.collection,
            d.created_by, u.nickname AS "created_by_nickname?", d.value, d.ctime_at, d.utime_at
        FROM rb_puzzle_store_doc d
        LEFT JOIN rb_user u ON u.id = d.created_by
        WHERE d.game_id = $1
            AND d.scope_type = $2
            AND d.team_id IS NOT DISTINCT FROM $3
            AND d.puzzle_id IS NOT DISTINCT FROM $4
            AND d.collection = $5
            AND (
                $6::BIGINT IS NULL
                OR ($15::BOOLEAN AND d.id < $6)
                OR (NOT $15::BOOLEAN AND d.id > $6)
            )
            AND (
                $7::BIGINT = 0
                OR (
                    SELECT COUNT(DISTINCT i.key)
                    FROM rb_puzzle_store_index i
                    WHERE i.doc_id = d.id
                        AND i.game_id = d.game_id
                        AND i.collection = d.collection
                        AND (
                            (i.key, i.value_text) IN (
                                SELECT * FROM UNNEST($8::TEXT[], $9::TEXT[])
                            )
                            OR (i.key, i.value_number) IN (
                                SELECT * FROM UNNEST($10::TEXT[], $11::DOUBLE PRECISION[])
                            )
                            OR (i.key, i.value_bool) IN (
                                SELECT * FROM UNNEST($12::TEXT[], $13::BOOLEAN[])
                            )
                        )
                ) = $7
            )
        ORDER BY
            CASE WHEN $15::BOOLEAN THEN d.id END DESC,
            CASE WHEN NOT $15::BOOLEAN THEN d.id END ASC
        LIMIT $14"#,
        game_id,
        scope_type,
        team_id,
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
    .fetch_all(&mut *conn)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| {
            store_doc_from_row(StoreDocRow {
                id: row.id,
                scope_type: row.scope_type,
                team_id: row.team_id,
                puzzle_id: row.puzzle_id,
                collection: row.collection,
                created_by: row.created_by,
                created_by_nickname: row.created_by_nickname,
                value: row.value,
                ctime_at: row.ctime_at,
                utime_at: row.utime_at,
            })
        })
        .collect())
}

pub async fn log_call(
    db_pool: &DbPool,
    input: PuzzleBackendCallLogInput<'_>,
) -> Result<(), RbInternalError> {
    sqlx::query!(
        r#"INSERT INTO rb_puzzle_backend_call_log
            (puzzle_id, team_id, user_id, execution_type, request_method,
                function_name, ok, duration_ms, submission_id, hint_id, error,
                console, console_truncated)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)"#,
        input.puzzle_id,
        input.team_id,
        input.user_id,
        input.execution_type,
        input.request_method,
        input.function_name,
        input.ok,
        input.duration_ms,
        input.submission_id,
        input.hint_id,
        input.error,
        input.console,
        input.console_truncated
    )
    .execute(db_pool)
    .await?;

    Ok(())
}

pub async fn list_call_logs(
    db_pool: &DbPool,
    query: PuzzleBackendCallLogQuery<'_>,
) -> Result<Vec<PuzzleBackendCallLog>, RbInternalError> {
    let rows = sqlx::query_as!(
        PuzzleBackendCallLog,
        r#"SELECT l.id, l.puzzle_id, l.team_id, t.name AS team_name,
            l.user_id, u.nickname AS user_nickname, l.execution_type,
            l.request_method, l.function_name, l.ok, l.duration_ms,
            l.submission_id, l.hint_id, l.error, l.console,
            l.console_truncated, l.ctime_at
        FROM rb_puzzle_backend_call_log l
        LEFT JOIN rb_team t ON t.id = l.team_id
        LEFT JOIN rb_user u ON u.id = l.user_id
        WHERE l.puzzle_id = $1
            AND ($2::TEXT IS NULL OR l.execution_type = $2)
            AND ($3::TEXT IS NULL OR l.function_name = $3)
            AND ($4::BOOLEAN IS NULL OR l.ok = $4)
            AND ($5::INT IS NULL OR l.team_id = $5)
            AND ($6::INT IS NULL OR l.user_id = $6)
            AND ($7::TIMESTAMPTZ IS NULL OR l.ctime_at >= $7)
            AND ($8::TIMESTAMPTZ IS NULL OR l.ctime_at <= $8)
        ORDER BY l.ctime_at DESC, l.id DESC
        LIMIT $9 OFFSET $10"#,
        query.puzzle_id,
        query.execution_type,
        query.function_name,
        query.ok,
        query.team_id,
        query.user_id,
        query.from,
        query.to,
        query.limit.clamp(1, 100),
        query.offset.max(0)
    )
    .fetch_all(db_pool)
    .await?;

    Ok(rows)
}

pub async fn count_call_logs(
    db_pool: &DbPool,
    query: &PuzzleBackendCallLogQuery<'_>,
) -> Result<i64, RbInternalError> {
    let count = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!"
        FROM rb_puzzle_backend_call_log l
        WHERE l.puzzle_id = $1
            AND ($2::TEXT IS NULL OR l.execution_type = $2)
            AND ($3::TEXT IS NULL OR l.function_name = $3)
            AND ($4::BOOLEAN IS NULL OR l.ok = $4)
            AND ($5::INT IS NULL OR l.team_id = $5)
            AND ($6::INT IS NULL OR l.user_id = $6)
            AND ($7::TIMESTAMPTZ IS NULL OR l.ctime_at >= $7)
            AND ($8::TIMESTAMPTZ IS NULL OR l.ctime_at <= $8)"#,
        query.puzzle_id,
        query.execution_type,
        query.function_name,
        query.ok,
        query.team_id,
        query.user_id,
        query.from,
        query.to
    )
    .fetch_one(db_pool)
    .await?;

    Ok(count)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sqlx::PgPool;

    use super::*;

    async fn create_game(pool: &PgPool) -> i32 {
        sqlx::query_scalar!(
            "INSERT INTO rb_game (title, settings) VALUES ('KV test', '{}') RETURNING id"
        )
        .fetch_one(pool)
        .await
        .expect("test game should be created")
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn atomic_kv_supports_claim_expiry_and_cas(pool: PgPool) {
        let game_id = create_game(&pool).await;
        let scope = BackendScope::Global;

        let first_pool = pool.clone();
        let second_pool = pool.clone();
        let (first, second) = tokio::join!(
            async move {
                set_kv_if_absent(
                    &first_pool,
                    game_id,
                    scope,
                    "concurrent",
                    &json!({ "owner": 1 }),
                    Some(60_000),
                )
                .await
                .expect("first claim should complete")
            },
            async move {
                set_kv_if_absent(
                    &second_pool,
                    game_id,
                    scope,
                    "concurrent",
                    &json!({ "owner": 2 }),
                    Some(60_000),
                )
                .await
                .expect("second claim should complete")
            }
        );
        assert_ne!(first.applied, second.applied);

        let claimed = if first.applied { first } else { second };
        let claimed_entry = claimed.entry.expect("claim should return its entry");
        assert_eq!(claimed_entry.version, 1);
        assert!(claimed_entry.expires_at > Some(claimed.server_time));

        let preserved_expiry = claimed_entry.expires_at;
        let updated = compare_and_set_kv(
            &pool,
            game_id,
            scope,
            "concurrent",
            claimed_entry.version,
            &json!({ "state": "updated" }),
            PuzzleBackendKvExpiry::Preserve,
        )
        .await
        .expect("CAS should complete");
        assert!(updated.applied);
        let updated_entry = updated.entry.expect("CAS should return its entry");
        assert_eq!(updated_entry.version, 2);
        assert_eq!(updated_entry.expires_at, preserved_expiry);

        let stale = compare_and_set_kv(
            &pool,
            game_id,
            scope,
            "concurrent",
            1,
            &json!({ "state": "stale" }),
            PuzzleBackendKvExpiry::Permanent,
        )
        .await
        .expect("stale CAS should complete");
        assert!(!stale.applied);
        assert_eq!(
            stale
                .entry
                .expect("current entry should be returned")
                .version,
            2
        );

        sqlx::query!("UPDATE rb_puzzle_kv SET expires_at = statement_timestamp() - INTERVAL '1 second' WHERE game_id = $1 AND key = 'concurrent'", game_id)
            .execute(&pool)
            .await
            .expect("entry should be expired");
        assert_eq!(
            get_kv(&pool, game_id, scope, "concurrent")
                .await
                .expect("expired read should complete"),
            None
        );
        let expired_cas = compare_and_set_kv(
            &pool,
            game_id,
            scope,
            "concurrent",
            2,
            &json!({ "state": "too late" }),
            PuzzleBackendKvExpiry::Preserve,
        )
        .await
        .expect("expired CAS should complete");
        assert!(!expired_cas.applied);
        assert!(expired_cas.entry.is_none());

        let reclaimed = set_kv_if_absent(
            &pool,
            game_id,
            scope,
            "concurrent",
            &json!({ "owner": 3 }),
            None,
        )
        .await
        .expect("expired entry should be reclaimable");
        assert!(reclaimed.applied);
        let reclaimed_entry = reclaimed.entry.expect("reclaim should return its entry");
        assert_eq!(reclaimed_entry.version, 3);
        assert_eq!(reclaimed_entry.expires_at, None);

        let reset_expiry = compare_and_set_kv(
            &pool,
            game_id,
            scope,
            "concurrent",
            3,
            &json!({ "owner": 3, "temporary": true }),
            PuzzleBackendKvExpiry::Ttl(60_000),
        )
        .await
        .expect("TTL CAS should complete");
        assert!(reset_expiry.applied);
        let reset_entry = reset_expiry.entry.expect("TTL CAS should return its entry");
        assert_eq!(reset_entry.version, 4);
        assert!(reset_entry.expires_at > Some(reset_expiry.server_time));

        let made_permanent = compare_and_set_kv(
            &pool,
            game_id,
            scope,
            "concurrent",
            4,
            &json!({ "owner": 3, "temporary": false }),
            PuzzleBackendKvExpiry::Permanent,
        )
        .await
        .expect("permanent CAS should complete");
        assert!(made_permanent.applied);
        let permanent_entry = made_permanent
            .entry
            .expect("permanent CAS should return its entry");
        assert_eq!(permanent_entry.version, 5);
        assert_eq!(permanent_entry.expires_at, None);

        set_kv(&pool, game_id, scope, "concurrent", &json!({ "owner": 4 }))
            .await
            .expect("unconditional set should complete");
        let final_entry = get_kv_entry(&pool, game_id, scope, "concurrent")
            .await
            .expect("final read should complete")
            .expect("final entry should exist");
        assert_eq!(final_entry.version, 6);
        assert_eq!(final_entry.expires_at, None);
    }
}
