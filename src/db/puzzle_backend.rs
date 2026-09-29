use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Connection, FromRow, PgConnection};
use time::OffsetDateTime;

use crate::{DbPool, error::RbInternalError};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize)]
#[serde(tag = "type")]
pub enum BackendScope {
    #[serde(rename = "global")]
    Global,
    #[serde(rename = "team")]
    Team {
        #[serde(rename = "teamId")]
        team_id: i32,
    },
    #[serde(rename = "puzzle")]
    Puzzle {
        #[serde(rename = "puzzleId")]
        puzzle_id: i32,
    },
    #[serde(rename = "teamPuzzle")]
    TeamPuzzle {
        #[serde(rename = "teamId")]
        team_id: i32,
        #[serde(rename = "puzzleId")]
        puzzle_id: i32,
    },
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
#[serde(rename_all = "camelCase")]
pub struct PuzzleStoreDoc {
    pub id: i64,
    pub scope: BackendScope,
    pub collection: String,
    pub created_by: Option<PuzzleStoreDocUser>,
    pub value: Value,
    #[serde(
        rename = "createdAt",
        with = "crate::serde_helpers::serialize_offset_datetime"
    )]
    pub ctime_at: OffsetDateTime,
    #[serde(
        rename = "updatedAt",
        with = "crate::serde_helpers::serialize_offset_datetime"
    )]
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

/// Proof that this game/scope pair was checked by the database.
#[derive(Clone, Copy)]
pub(crate) struct ValidatedBackendScope {
    game_id: i32,
    scope: BackendScope,
}

pub(crate) async fn validate_backend_scope_conn(
    conn: &mut PgConnection,
    game_id: i32,
    scope: BackendScope,
) -> Result<ValidatedBackendScope, RbInternalError> {
    require_scope_in_game_conn(conn, game_id, scope).await?;
    Ok(ValidatedBackendScope { game_id, scope })
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
    let checked = validate_backend_scope_conn(conn, game_id, scope).await?;
    get_kv_validated_conn(conn, &checked, key).await
}

pub(crate) async fn get_kv_validated_conn(
    conn: &mut PgConnection,
    checked: &ValidatedBackendScope,
    key: &str,
) -> Result<Option<Value>, RbInternalError> {
    Ok(get_kv_entry_validated_conn(conn, checked, key)
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
    let checked = validate_backend_scope_conn(conn, game_id, scope).await?;
    get_kv_entry_validated_conn(conn, &checked, key).await
}

pub(crate) async fn get_kv_entry_validated_conn(
    conn: &mut PgConnection,
    checked: &ValidatedBackendScope,
    key: &str,
) -> Result<Option<PuzzleBackendKvValue>, RbInternalError> {
    let game_id = checked.game_id;
    let scope = checked.scope;
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
    expiry: PuzzleBackendKvExpiry,
) -> Result<PuzzleBackendKvMutation, RbInternalError> {
    let mut conn = db_pool.acquire().await?;
    set_kv_conn(&mut conn, game_id, scope, key, value, expiry).await
}

pub async fn set_kv_conn(
    conn: &mut PgConnection,
    game_id: i32,
    scope: BackendScope,
    key: &str,
    value: &Value,
    expiry: PuzzleBackendKvExpiry,
) -> Result<PuzzleBackendKvMutation, RbInternalError> {
    let checked = validate_backend_scope_conn(conn, game_id, scope).await?;
    set_kv_validated_conn(conn, &checked, key, value, expiry).await
}

pub(crate) async fn set_kv_validated_conn(
    conn: &mut PgConnection,
    checked: &ValidatedBackendScope,
    key: &str,
    value: &Value,
    expiry: PuzzleBackendKvExpiry,
) -> Result<PuzzleBackendKvMutation, RbInternalError> {
    let game_id = checked.game_id;
    let scope = checked.scope;
    let (scope_type, team_id, puzzle_id) = scope.parts();
    let (preserve_expiry, ttl_ms) = match expiry {
        PuzzleBackendKvExpiry::Preserve => (true, None),
        PuzzleBackendKvExpiry::Permanent => (false, None),
        PuzzleBackendKvExpiry::Ttl(ttl_ms) => (false, Some(ttl_ms)),
    };
    let entry = sqlx::query_as!(
        PuzzleBackendKvValue,
        r#"INSERT INTO rb_puzzle_kv (
            game_id, scope_type, team_id, puzzle_id, key, value, expires_at
        )
        VALUES (
            $1, $2, $3, $4, $5, $6,
            CASE WHEN $8::BIGINT IS NULL THEN NULL
                ELSE statement_timestamp() + $8 * INTERVAL '1 millisecond' END
        )
        ON CONFLICT (game_id, scope_type, team_id, puzzle_id, key)
        DO UPDATE SET value = EXCLUDED.value,
            version = rb_puzzle_kv.version + 1,
            expires_at = CASE
                WHEN $7 THEN rb_puzzle_kv.expires_at
                ELSE EXCLUDED.expires_at
            END,
            utime_at = CURRENT_TIMESTAMP
        RETURNING value, version, expires_at"#,
        game_id,
        scope_type,
        team_id,
        puzzle_id,
        key,
        value,
        preserve_expiry,
        ttl_ms
    )
    .fetch_one(&mut *conn)
    .await?;

    current_kv_mutation_conn(conn, checked, key, Some(entry)).await
}

pub async fn increment_kv(
    db_pool: &DbPool,
    game_id: i32,
    scope: BackendScope,
    key: &str,
    amount: f64,
    expiry: PuzzleBackendKvExpiry,
) -> Result<PuzzleBackendKvMutation, RbInternalError> {
    let mut conn = db_pool.acquire().await?;
    increment_kv_conn(&mut conn, game_id, scope, key, amount, expiry).await
}

pub async fn increment_kv_conn(
    conn: &mut PgConnection,
    game_id: i32,
    scope: BackendScope,
    key: &str,
    amount: f64,
    expiry: PuzzleBackendKvExpiry,
) -> Result<PuzzleBackendKvMutation, RbInternalError> {
    let checked = validate_backend_scope_conn(conn, game_id, scope).await?;
    increment_kv_validated_conn(conn, &checked, key, amount, expiry).await
}

pub(crate) async fn increment_kv_validated_conn(
    conn: &mut PgConnection,
    checked: &ValidatedBackendScope,
    key: &str,
    amount: f64,
    expiry: PuzzleBackendKvExpiry,
) -> Result<PuzzleBackendKvMutation, RbInternalError> {
    let game_id = checked.game_id;
    let scope = checked.scope;
    if !amount.is_finite() {
        return Err(RbInternalError::Other(
            "KV increment amount must be a finite number".to_string(),
        ));
    }
    let (scope_type, team_id, puzzle_id) = scope.parts();
    let (preserve_expiry, ttl_ms) = match expiry {
        PuzzleBackendKvExpiry::Preserve => (true, None),
        PuzzleBackendKvExpiry::Permanent => (false, None),
        PuzzleBackendKvExpiry::Ttl(ttl_ms) => (false, Some(ttl_ms)),
    };
    let entry = sqlx::query_as!(
        PuzzleBackendKvValue,
        r#"INSERT INTO rb_puzzle_kv (
            game_id, scope_type, team_id, puzzle_id, key, value, expires_at
        )
        VALUES (
            $1, $2, $3, $4, $5, to_jsonb($6::DOUBLE PRECISION),
            CASE WHEN $8::BIGINT IS NULL THEN NULL
                ELSE statement_timestamp() + $8 * INTERVAL '1 millisecond' END
        )
        ON CONFLICT (game_id, scope_type, team_id, puzzle_id, key)
        DO UPDATE SET
            value = CASE
                WHEN rb_puzzle_kv.expires_at IS NOT NULL
                    AND rb_puzzle_kv.expires_at <= statement_timestamp()
                    THEN EXCLUDED.value
                WHEN (rb_puzzle_kv.value #>> '{}')::DOUBLE PRECISION
                    + $6 BETWEEN -1.7976931348623157e308::DOUBLE PRECISION
                        AND 1.7976931348623157e308::DOUBLE PRECISION
                    THEN to_jsonb(
                        (rb_puzzle_kv.value #>> '{}')::DOUBLE PRECISION + $6
                    )
                ELSE NULL
            END,
            version = rb_puzzle_kv.version + 1,
            expires_at = CASE
                WHEN rb_puzzle_kv.expires_at IS NOT NULL
                    AND rb_puzzle_kv.expires_at <= statement_timestamp()
                    THEN EXCLUDED.expires_at
                WHEN $7 THEN rb_puzzle_kv.expires_at
                ELSE EXCLUDED.expires_at
            END,
            utime_at = CURRENT_TIMESTAMP
        RETURNING value, version, expires_at"#,
        game_id,
        scope_type,
        team_id,
        puzzle_id,
        key,
        amount,
        preserve_expiry,
        ttl_ms
    )
    .fetch_one(&mut *conn)
    .await?;

    current_kv_mutation_conn(conn, checked, key, Some(entry)).await
}

async fn current_kv_mutation_conn(
    conn: &mut PgConnection,
    checked: &ValidatedBackendScope,
    key: &str,
    applied_entry: Option<PuzzleBackendKvValue>,
) -> Result<PuzzleBackendKvMutation, RbInternalError> {
    let applied = applied_entry.is_some();
    let entry = if applied {
        applied_entry
    } else {
        get_kv_entry_validated_conn(conn, checked, key).await?
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
    let checked = validate_backend_scope_conn(conn, game_id, scope).await?;
    set_kv_if_absent_validated_conn(conn, &checked, key, value, ttl_ms).await
}

pub(crate) async fn set_kv_if_absent_validated_conn(
    conn: &mut PgConnection,
    checked: &ValidatedBackendScope,
    key: &str,
    value: &Value,
    ttl_ms: Option<i64>,
) -> Result<PuzzleBackendKvMutation, RbInternalError> {
    let game_id = checked.game_id;
    let scope = checked.scope;
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

    current_kv_mutation_conn(conn, checked, key, entry).await
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
    let checked = validate_backend_scope_conn(conn, game_id, scope).await?;
    compare_and_set_kv_validated_conn(conn, &checked, key, expected_version, value, expiry).await
}

pub(crate) async fn compare_and_set_kv_validated_conn(
    conn: &mut PgConnection,
    checked: &ValidatedBackendScope,
    key: &str,
    expected_version: i64,
    value: &Value,
    expiry: PuzzleBackendKvExpiry,
) -> Result<PuzzleBackendKvMutation, RbInternalError> {
    let game_id = checked.game_id;
    let scope = checked.scope;
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

    current_kv_mutation_conn(conn, checked, key, entry).await
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
    let checked = validate_backend_scope_conn(conn, game_id, scope).await?;
    delete_kv_validated_conn(conn, &checked, key).await
}

pub(crate) async fn delete_kv_validated_conn(
    conn: &mut PgConnection,
    checked: &ValidatedBackendScope,
    key: &str,
) -> Result<bool, RbInternalError> {
    let game_id = checked.game_id;
    let scope = checked.scope;
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
    let checked = validate_backend_scope_conn(conn, game_id, scope).await?;
    insert_store_doc_validated_conn(conn, &checked, collection, created_by, value, indexes).await
}

pub(crate) async fn insert_store_doc_validated_conn(
    conn: &mut PgConnection,
    checked: &ValidatedBackendScope,
    collection: &str,
    created_by: i32,
    value: &Value,
    indexes: &[PuzzleStoreIndexEntry],
) -> Result<PuzzleStoreDoc, RbInternalError> {
    let game_id = checked.game_id;
    let scope = checked.scope;
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
    let checked = validate_backend_scope_conn(conn, game_id, scope).await?;
    get_store_doc_validated_conn(conn, &checked, collection, doc_id).await
}

pub(crate) async fn get_store_doc_validated_conn(
    conn: &mut PgConnection,
    checked: &ValidatedBackendScope,
    collection: &str,
    doc_id: i64,
) -> Result<Option<PuzzleStoreDoc>, RbInternalError> {
    let game_id = checked.game_id;
    let scope = checked.scope;
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
    let checked = validate_backend_scope_conn(conn, game_id, scope).await?;
    list_store_docs_validated_conn(conn, &checked, collection, options).await
}

pub(crate) async fn list_store_docs_validated_conn(
    conn: &mut PgConnection,
    checked: &ValidatedBackendScope,
    collection: &str,
    options: &PuzzleStoreListOptions,
) -> Result<Vec<PuzzleStoreDoc>, RbInternalError> {
    let game_id = checked.game_id;
    let scope = checked.scope;
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

struct BatchKvRow {
    key: String,
    value: Value,
    version: i64,
    expires_at: Option<OffsetDateTime>,
}

pub(crate) async fn get_kv_entries_validated_conn(
    conn: &mut PgConnection,
    checked: &ValidatedBackendScope,
    keys: &[String],
) -> Result<Vec<Option<PuzzleBackendKvValue>>, RbInternalError> {
    if keys.is_empty() {
        return Ok(vec![]);
    }
    let (scope_type, team_id, puzzle_id) = checked.scope.parts();
    let rows = sqlx::query_as!(
        BatchKvRow,
        "SELECT key, value, version, expires_at FROM rb_puzzle_kv
         WHERE game_id = $1 AND scope_type = $2
         AND team_id IS NOT DISTINCT FROM $3 AND puzzle_id IS NOT DISTINCT FROM $4
         AND key = ANY($5) AND (expires_at IS NULL OR expires_at > statement_timestamp())",
        checked.game_id,
        scope_type,
        team_id,
        puzzle_id,
        keys,
    )
    .fetch_all(conn)
    .await?;
    let entries: std::collections::HashMap<_, _> = rows
        .into_iter()
        .map(|row| {
            (
                row.key,
                PuzzleBackendKvValue {
                    value: row.value,
                    version: row.version,
                    expires_at: row.expires_at,
                },
            )
        })
        .collect();
    Ok(keys.iter().map(|key| entries.get(key).cloned()).collect())
}

pub(crate) struct BackendCommitWrite {
    pub key: String,
    pub expected_version: Option<i64>,
    pub value: Value,
    pub expiry: PuzzleBackendKvExpiry,
}

pub(crate) struct BackendCommitInsert {
    pub collection: String,
    pub value: Value,
    pub indexes: Vec<PuzzleStoreIndexEntry>,
}

pub(crate) struct BackendCommitResult {
    pub applied: bool,
    pub entries: Vec<Option<PuzzleBackendKvValue>>,
    pub documents: Vec<PuzzleStoreDoc>,
    pub server_time: OffsetDateTime,
}

// Begin on the connection, rather than the pool: SQLx creates a savepoint
// when the caller already holds a judge transaction.
pub(crate) async fn insert_store_doc_atomic_conn(
    conn: &mut PgConnection,
    checked: &ValidatedBackendScope,
    collection: &str,
    created_by: i32,
    value: &Value,
    indexes: &[PuzzleStoreIndexEntry],
) -> Result<PuzzleStoreDoc, RbInternalError> {
    let mut tx = conn.begin().await?;
    let doc =
        insert_store_doc_validated_conn(&mut tx, checked, collection, created_by, value, indexes)
            .await?;
    tx.commit().await?;
    Ok(doc)
}

fn ensure_commit_deadline(
    deadline: std::time::Instant,
) -> Result<std::time::Duration, RbInternalError> {
    deadline
        .checked_duration_since(std::time::Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| RbInternalError::Other("backend function execution timed out".into()))
}

fn bounded_timeout(current: &str, budget_ms: u128) -> String {
    // PostgreSQL renders these settings as an integer and a unit, or 0 (disabled).
    let split = current
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(current.len());
    let amount = current[..split].parse::<u128>().unwrap_or(0);
    let multiplier = match &current[split..] {
        "s" => 1000,
        "min" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => 1,
    };
    let existing_ms = amount.saturating_mul(multiplier);
    format!(
        "{}ms",
        if existing_ms == 0 {
            budget_ms
        } else {
            existing_ms.min(budget_ms)
        }
        .max(1)
    )
}

pub(crate) async fn commit_backend_conn(
    conn: &mut PgConnection,
    checked: &ValidatedBackendScope,
    writes: &[BackendCommitWrite],
    inserts: &[BackendCommitInsert],
    created_by: i32,
    deadline: std::time::Instant,
) -> Result<BackendCommitResult, RbInternalError> {
    let budget = ensure_commit_deadline(deadline)?.as_millis().max(1);
    let mut tx = conn.begin().await?;
    let previous = sqlx::query!(
        r#"SELECT current_setting('statement_timeout') AS "statement_timeout!",
                  current_setting('lock_timeout') AS "lock_timeout!""#,
    )
    .fetch_one(&mut *tx)
    .await?;
    let statement_timeout = bounded_timeout(&previous.statement_timeout, budget);
    let lock_timeout = bounded_timeout(&previous.lock_timeout, budget.min(500));
    sqlx::query!(
        r#"SELECT set_config('statement_timeout', $1, true) AS "statement_timeout!",
                  set_config('lock_timeout', $2, true) AS "lock_timeout!""#,
        statement_timeout,
        lock_timeout,
    )
    .fetch_one(&mut *tx)
    .await?;
    let mut order: Vec<usize> = (0..writes.len()).collect();
    order.sort_by(|a, b| writes[*a].key.cmp(&writes[*b].key));
    let mut entries = vec![None; writes.len()];
    for index in order {
        ensure_commit_deadline(deadline)?;
        let write = &writes[index];
        let mutation = match write.expected_version {
            Some(version) => {
                compare_and_set_kv_validated_conn(
                    &mut tx,
                    checked,
                    &write.key,
                    version,
                    &write.value,
                    write.expiry,
                )
                .await?
            }
            None => {
                set_kv_if_absent_validated_conn(
                    &mut tx,
                    checked,
                    &write.key,
                    &write.value,
                    match write.expiry {
                        PuzzleBackendKvExpiry::Ttl(ms) => Some(ms),
                        _ => None,
                    },
                )
                .await?
            }
        };
        if !mutation.applied {
            tx.rollback().await?;
            ensure_commit_deadline(deadline)?;
            let keys: Vec<_> = writes.iter().map(|write| write.key.clone()).collect();
            let entries = get_kv_entries_validated_conn(conn, checked, &keys).await?;
            return Ok(BackendCommitResult {
                applied: false,
                entries,
                documents: vec![],
                server_time: OffsetDateTime::now_utc(),
            });
        }
        entries[index] = mutation.entry;
    }
    let mut documents = Vec::with_capacity(inserts.len());
    for insert in inserts {
        ensure_commit_deadline(deadline)?;
        documents.push(
            insert_store_doc_validated_conn(
                &mut tx,
                checked,
                &insert.collection,
                created_by,
                &insert.value,
                &insert.indexes,
            )
            .await?,
        );
    }
    // Restore local settings before releasing a judge savepoint.
    sqlx::query!(
        r#"SELECT set_config('statement_timeout', $1, true) AS "statement_timeout!",
                  set_config('lock_timeout', $2, true) AS "lock_timeout!""#,
        previous.statement_timeout,
        previous.lock_timeout,
    )
    .fetch_one(&mut *tx)
    .await?;
    ensure_commit_deadline(deadline)?;
    tx.commit().await?;
    Ok(BackendCommitResult {
        applied: true,
        entries,
        documents,
        server_time: OffsetDateTime::now_utc(),
    })
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    use serde_json::json;
    use sqlx::PgPool;
    use std::time::{Duration, Instant};

    async fn fixture(pool: &PgPool) -> (i32, i32, BackendScope) {
        let game: i32 = sqlx::query_scalar!(
            "INSERT INTO rb_game (title, settings) VALUES ('Batch test', '{}') RETURNING id"
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let user: i32 = sqlx::query_scalar!("INSERT INTO rb_user (email, pass, nickname) VALUES ('batch@example.invalid', '', 'Tester') RETURNING id").fetch_one(pool).await.unwrap();
        let team: i32 = sqlx::query_scalar!("INSERT INTO rb_team (name, pass, bio, game_id) VALUES ('Batch team', '', '', $1) RETURNING id" , game).fetch_one(pool).await.unwrap();
        (game, user, BackendScope::Team { team_id: team })
    }

    fn write(key: &str, version: Option<i64>, value: Value) -> BackendCommitWrite {
        BackendCommitWrite {
            key: key.into(),
            expected_version: version,
            value,
            expiry: PuzzleBackendKvExpiry::Permanent,
        }
    }

    fn insert() -> BackendCommitInsert {
        BackendCommitInsert {
            collection: "attempts".into(),
            value: json!({"puzzleId": "a", "answer": "wrong"}),
            indexes: vec![PuzzleStoreIndexEntry {
                key: "puzzleId".into(),
                value: PuzzleStoreIndexValue::Text("a".into()),
            }],
        }
    }

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn batch_read_orders_duplicates_expiry_and_scope(pool: PgPool) {
        let (game, _, scope) = fixture(&pool).await;
        set_kv(
            &pool,
            game,
            scope,
            "a",
            &json!(1),
            PuzzleBackendKvExpiry::Permanent,
        )
        .await
        .unwrap();
        set_kv(
            &pool,
            game,
            scope,
            "b",
            &json!(2),
            PuzzleBackendKvExpiry::Permanent,
        )
        .await
        .unwrap();
        sqlx::query!("UPDATE rb_puzzle_kv SET expires_at = NOW() - INTERVAL '1 second' WHERE game_id = $1 AND key = 'b'" , game).execute(&pool).await.unwrap();
        let mut conn = pool.acquire().await.unwrap();
        let checked = validate_backend_scope_conn(&mut conn, game, scope)
            .await
            .unwrap();
        let entries = get_kv_entries_validated_conn(
            &mut conn,
            &checked,
            &["b".into(), "a".into(), "missing".into(), "a".into()],
        )
        .await
        .unwrap();
        assert!(entries[0].is_none());
        assert!(entries[2].is_none());
        assert_eq!(entries[1].as_ref().unwrap().value, json!(1));
        assert_eq!(entries[3].as_ref().unwrap().version, 1);
        assert!(
            get_kv_entries_validated_conn(&mut conn, &checked, &[])
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            validate_backend_scope_conn(&mut conn, game + 1, scope)
                .await
                .is_err()
        );
        let other = validate_backend_scope_conn(&mut conn, game, BackendScope::Global)
            .await
            .unwrap();
        assert!(
            get_kv_entries_validated_conn(&mut conn, &other, &["a".into()])
                .await
                .unwrap()[0]
                .is_none()
        );
        let other_team: i32 = sqlx::query_scalar!("INSERT INTO rb_team (name, pass, bio, game_id) VALUES ('Other', '', '', $1) RETURNING id" , game).fetch_one(&mut *conn).await.unwrap();
        let other = validate_backend_scope_conn(
            &mut conn,
            game,
            BackendScope::Team {
                team_id: other_team,
            },
        )
        .await
        .unwrap();
        assert!(
            get_kv_entries_validated_conn(&mut conn, &other, &["a".into()])
                .await
                .unwrap()[0]
                .is_none()
        );
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn commit_success_conflict_and_insert_failure_are_atomic(pool: PgPool) {
        let (game, user, scope) = fixture(&pool).await;
        let mut conn = pool.acquire().await.unwrap();
        let checked = validate_backend_scope_conn(&mut conn, game, scope)
            .await
            .unwrap();
        let first = commit_backend_conn(
            &mut conn,
            &checked,
            &[write("b", None, json!(2)), write("a", None, json!(1))],
            &[insert()],
            user,
            deadline(),
        )
        .await
        .unwrap();
        assert!(first.applied);
        assert_eq!(first.entries[0].as_ref().unwrap().value, json!(2));
        assert_eq!(first.documents.len(), 1);
        // a changes first, b then conflicts: a must be rolled back too.
        let conflict = commit_backend_conn(
            &mut conn,
            &checked,
            &[
                write("a", Some(1), json!(10)),
                write("b", Some(99), json!(20)),
            ],
            &[insert()],
            user,
            deadline(),
        )
        .await
        .unwrap();
        assert!(!conflict.applied);
        assert!(conflict.documents.is_empty());
        assert_eq!(conflict.entries[0].as_ref().unwrap().value, json!(1));
        let mut bad = insert();
        bad.indexes[0].value = PuzzleStoreIndexValue::Text("\0".into());
        // Failure occurs after the KV update and document INSERT, inside the index INSERT.
        assert!(
            commit_backend_conn(
                &mut conn,
                &checked,
                &[write("a", Some(1), json!(10))],
                &[insert(), bad],
                user,
                deadline()
            )
            .await
            .is_err()
        );
        assert_eq!(
            get_kv_entry_validated_conn(&mut conn, &checked, "a")
                .await
                .unwrap()
                .unwrap()
                .version,
            1
        );
        let count: i64 =
            sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM rb_puzzle_store_doc"#)
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(count, 1);
        let count: i64 =
            sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM rb_puzzle_store_index"#)
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(count, 1);
        assert!(
            commit_backend_conn(
                &mut conn,
                &checked,
                &[write("a", Some(1), json!(10))],
                &[insert()],
                user,
                Instant::now() - Duration::from_millis(1)
            )
            .await
            .is_err()
        );
        assert_eq!(
            get_kv_entry_validated_conn(&mut conn, &checked, "a")
                .await
                .unwrap()
                .unwrap()
                .version,
            1
        );
        let inserted = commit_backend_conn(
            &mut conn,
            &checked,
            &[],
            &[insert(), insert()],
            user,
            deadline(),
        )
        .await
        .unwrap();
        assert!(inserted.applied);
        assert!(inserted.entries.is_empty());
        assert_eq!(inserted.documents.len(), 2);
        assert!(inserted.documents[0].id < inserted.documents[1].id);
        let mut bad = insert();
        bad.indexes[0].value = PuzzleStoreIndexValue::Text("\0".into());
        assert!(
            commit_backend_conn(&mut conn, &checked, &[], &[insert(), bad], user, deadline())
                .await
                .is_err()
        );
        let count = sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM rb_puzzle_store_doc"#)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(count, 3);
        let count =
            sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM rb_puzzle_store_index"#)
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(count, 3);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn commit_savepoint_preserves_outer_transaction_and_settings(pool: PgPool) {
        let (game, user, scope) = fixture(&pool).await;
        let mut outer = pool.begin().await.unwrap();
        let checked = validate_backend_scope_conn(&mut outer, game, scope)
            .await
            .unwrap();
        sqlx::query!("SET LOCAL statement_timeout = '500ms'")
            .execute(&mut *outer)
            .await
            .unwrap();
        let first = commit_backend_conn(
            &mut outer,
            &checked,
            &[write("a", None, json!(1))],
            &[insert()],
            user,
            deadline(),
        )
        .await
        .unwrap();
        assert!(first.applied);
        let conflict = commit_backend_conn(
            &mut outer,
            &checked,
            &[write("a", Some(99), json!(2))],
            &[insert()],
            user,
            deadline(),
        )
        .await
        .unwrap();
        assert!(!conflict.applied);
        let mut bad = insert();
        bad.indexes[0].value = PuzzleStoreIndexValue::Text("\0".into());
        assert!(
            commit_backend_conn(
                &mut outer,
                &checked,
                &[write("a", Some(1), json!(2))],
                &[bad],
                user,
                deadline()
            )
            .await
            .is_err()
        );
        let value = get_kv_entry_validated_conn(&mut outer, &checked, "a")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(value.version, 1);
        let timeout: String =
            sqlx::query_scalar!(r#"SELECT current_setting('statement_timeout') AS "timeout!""#)
                .fetch_one(&mut *outer)
                .await
                .unwrap();
        assert_eq!(timeout, "500ms");
        outer.rollback().await.unwrap();
        assert!(
            get_kv_entry(&pool, game, scope, "a")
                .await
                .unwrap()
                .is_none()
        );
        let count: i64 =
            sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM rb_puzzle_store_doc"#)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 0);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn commit_concurrent_batches_have_one_winner(pool: PgPool) {
        let (game, user, scope) = fixture(&pool).await;
        for key in ["a", "b"] {
            set_kv(
                &pool,
                game,
                scope,
                key,
                &json!(0),
                PuzzleBackendKvExpiry::Permanent,
            )
            .await
            .unwrap();
        }
        let run = |reverse: bool| {
            let pool = pool.clone();
            async move {
                let mut conn = pool.acquire().await.unwrap();
                let checked = validate_backend_scope_conn(&mut conn, game, scope)
                    .await
                    .unwrap();
                let mut writes = vec![write("a", Some(1), json!(1)), write("b", Some(1), json!(2))];
                if reverse {
                    writes.reverse();
                }
                commit_backend_conn(&mut conn, &checked, &writes, &[insert()], user, deadline())
                    .await
                    .unwrap()
            }
        };
        let (a, b) = tokio::join!(run(false), run(true));
        assert_ne!(a.applied, b.applied);
        for key in ["a", "b"] {
            assert_eq!(
                get_kv_entry(&pool, game, scope, key)
                    .await
                    .unwrap()
                    .unwrap()
                    .version,
                2
            );
        }
        let count: i64 =
            sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM rb_puzzle_store_doc"#)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 1);
        let run = || {
            let pool = pool.clone();
            async move {
                let mut conn = pool.acquire().await.unwrap();
                let checked = validate_backend_scope_conn(&mut conn, game, scope)
                    .await
                    .unwrap();
                commit_backend_conn(
                    &mut conn,
                    &checked,
                    &[write("new", None, json!(1))],
                    &[insert()],
                    user,
                    deadline(),
                )
                .await
                .unwrap()
            }
        };
        let (a, b) = tokio::join!(run(), run());
        assert_ne!(a.applied, b.applied);
        let count: i64 =
            sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM rb_puzzle_store_doc"#)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 2);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn commit_expiry_and_ttl_match_single_key_operations(pool: PgPool) {
        let (game, user, scope) = fixture(&pool).await;
        set_kv(
            &pool,
            game,
            scope,
            "a",
            &json!(0),
            PuzzleBackendKvExpiry::Ttl(60_000),
        )
        .await
        .unwrap();
        let mut conn = pool.acquire().await.unwrap();
        let checked = validate_backend_scope_conn(&mut conn, game, scope)
            .await
            .unwrap();
        let before = get_kv_entry_validated_conn(&mut conn, &checked, "a")
            .await
            .unwrap()
            .unwrap();
        let mut keep = write("a", Some(1), json!(1));
        keep.expiry = PuzzleBackendKvExpiry::Preserve;
        let result = commit_backend_conn(&mut conn, &checked, &[keep], &[], user, deadline())
            .await
            .unwrap();
        assert_eq!(
            result.entries[0].as_ref().unwrap().expires_at,
            before.expires_at
        );
        sqlx::query!(
            "UPDATE rb_puzzle_kv SET expires_at = NOW() - INTERVAL '1 second' WHERE game_id = $1",
            game
        )
        .execute(&mut *conn)
        .await
        .unwrap();
        assert!(
            !commit_backend_conn(
                &mut conn,
                &checked,
                &[write("a", Some(2), json!(2))],
                &[],
                user,
                deadline()
            )
            .await
            .unwrap()
            .applied
        );
        let mut reclaim = write("a", None, json!(3));
        reclaim.expiry = PuzzleBackendKvExpiry::Ttl(60_000);
        let result = commit_backend_conn(&mut conn, &checked, &[reclaim], &[], user, deadline())
            .await
            .unwrap();
        assert!(result.applied);
        assert_eq!(result.entries[0].as_ref().unwrap().version, 3);
        assert!(result.entries[0].as_ref().unwrap().expires_at.is_some());
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn commit_lock_wait_is_bounded_and_rolls_back(pool: PgPool) {
        let (game, user, scope) = fixture(&pool).await;
        for key in ["a", "b"] {
            set_kv(
                &pool,
                game,
                scope,
                key,
                &json!(0),
                PuzzleBackendKvExpiry::Permanent,
            )
            .await
            .unwrap();
        }
        let mut blocker = pool.begin().await.unwrap();
        sqlx::query!(
            "SELECT key FROM rb_puzzle_kv WHERE game_id = $1 AND key = 'b' FOR UPDATE",
            game
        )
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
        let mut conn = pool.acquire().await.unwrap();
        let checked = validate_backend_scope_conn(&mut conn, game, scope)
            .await
            .unwrap();
        let result = commit_backend_conn(
            &mut conn,
            &checked,
            &[write("a", Some(1), json!(1)), write("b", Some(1), json!(1))],
            &[insert()],
            user,
            Instant::now() + Duration::from_millis(100),
        )
        .await;
        assert!(result.is_err());
        blocker.rollback().await.unwrap();
        assert_eq!(
            get_kv_entry_validated_conn(&mut conn, &checked, "a")
                .await
                .unwrap()
                .unwrap()
                .version,
            1
        );
        let count: i64 =
            sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM rb_puzzle_store_doc"#)
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn timeouts_preserve_stricter_existing_limits() {
        assert_eq!(bounded_timeout("500ms", 5000), "500ms");
        assert_eq!(bounded_timeout("0", 100), "100ms");
        assert_eq!(bounded_timeout("1min", 5000), "5000ms");
    }
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

    #[test]
    fn store_documents_and_scopes_serialize_with_public_names() {
        let document = PuzzleStoreDoc {
            id: 1,
            scope: BackendScope::TeamPuzzle {
                team_id: 2,
                puzzle_id: 3,
            },
            collection: "attempts".to_string(),
            created_by: Some(PuzzleStoreDocUser {
                id: 4,
                nickname: "User".to_string(),
            }),
            value: json!({ "score": 5 }),
            ctime_at: OffsetDateTime::UNIX_EPOCH,
            utime_at: OffsetDateTime::UNIX_EPOCH,
        };

        let value = serde_json::to_value(document).expect("document should serialize");
        assert_eq!(
            value["scope"],
            json!({ "type": "teamPuzzle", "teamId": 2, "puzzleId": 3 })
        );
        assert_eq!(value["createdBy"]["id"], 4);
        assert!(value.get("createdAt").is_some());
        assert!(value.get("updatedAt").is_some());
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

        let set = set_kv(
            &pool,
            game_id,
            scope,
            "concurrent",
            &json!({ "owner": 4 }),
            PuzzleBackendKvExpiry::Preserve,
        )
        .await
        .expect("unconditional set should complete");
        assert!(set.applied);
        assert_eq!(
            set.entry
                .as_ref()
                .expect("set should return its entry")
                .version,
            6
        );
        let final_entry = get_kv_entry(&pool, game_id, scope, "concurrent")
            .await
            .expect("final read should complete")
            .expect("final entry should exist");
        assert_eq!(final_entry.version, 6);
        assert_eq!(final_entry.expires_at, None);

        let counter = set_kv(
            &pool,
            game_id,
            scope,
            "counter",
            &json!(10),
            PuzzleBackendKvExpiry::Ttl(60_000),
        )
        .await
        .expect("counter set should complete");
        let counter_expiry = counter
            .entry
            .expect("counter set should return its entry")
            .expires_at;
        let incremented = increment_kv(
            &pool,
            game_id,
            scope,
            "counter",
            2.5,
            PuzzleBackendKvExpiry::Preserve,
        )
        .await
        .expect("increment should complete");
        assert!(incremented.applied);
        let incremented_entry = incremented
            .entry
            .expect("increment should return its entry");
        assert_eq!(incremented_entry.value, json!(12.5));
        assert_eq!(incremented_entry.version, 2);
        assert_eq!(incremented_entry.expires_at, counter_expiry);

        let overwritten = set_kv(
            &pool,
            game_id,
            scope,
            "counter",
            &json!(20),
            PuzzleBackendKvExpiry::Preserve,
        )
        .await
        .expect("set without TTL should preserve expiry");
        let overwritten_entry = overwritten
            .entry
            .expect("set should return the overwritten entry");
        assert_eq!(overwritten_entry.version, 3);
        assert_eq!(overwritten_entry.expires_at, counter_expiry);

        let first_pool = pool.clone();
        let second_pool = pool.clone();
        let (first, second) = tokio::join!(
            increment_kv(
                &first_pool,
                game_id,
                scope,
                "parallel_counter",
                1.0,
                PuzzleBackendKvExpiry::Preserve,
            ),
            increment_kv(
                &second_pool,
                game_id,
                scope,
                "parallel_counter",
                1.0,
                PuzzleBackendKvExpiry::Preserve,
            ),
        );
        first.expect("first parallel increment should complete");
        second.expect("second parallel increment should complete");
        assert_eq!(
            get_kv(&pool, game_id, scope, "parallel_counter")
                .await
                .expect("parallel counter read should complete"),
            Some(json!(2))
        );

        sqlx::query!(
            "UPDATE rb_puzzle_kv SET expires_at = statement_timestamp() - INTERVAL '1 second' WHERE game_id = $1 AND key = 'counter'",
            game_id
        )
        .execute(&pool)
        .await
        .expect("counter should be expired");
        let reset_counter = increment_kv(
            &pool,
            game_id,
            scope,
            "counter",
            3.0,
            PuzzleBackendKvExpiry::Preserve,
        )
        .await
        .expect("expired counter increment should complete");
        let reset_entry = reset_counter
            .entry
            .expect("reset increment should return its entry");
        assert_eq!(reset_entry.value, json!(3));
        assert_eq!(reset_entry.version, 4);
        assert_eq!(reset_entry.expires_at, None);

        let temporary_increment = increment_kv(
            &pool,
            game_id,
            scope,
            "parallel_counter",
            1.0,
            PuzzleBackendKvExpiry::Ttl(60_000),
        )
        .await
        .expect("increment should set TTL");
        assert!(
            temporary_increment
                .entry
                .expect("increment should return its entry")
                .expires_at
                > Some(temporary_increment.server_time)
        );
        let permanent_increment = increment_kv(
            &pool,
            game_id,
            scope,
            "parallel_counter",
            1.0,
            PuzzleBackendKvExpiry::Permanent,
        )
        .await
        .expect("increment should clear TTL");
        assert_eq!(
            permanent_increment
                .entry
                .expect("increment should return its entry")
                .expires_at,
            None
        );

        set_kv(
            &pool,
            game_id,
            scope,
            "not_a_number",
            &json!({ "value": 1 }),
            PuzzleBackendKvExpiry::Preserve,
        )
        .await
        .expect("non-number set should complete");
        assert!(
            increment_kv(
                &pool,
                game_id,
                scope,
                "not_a_number",
                1.0,
                PuzzleBackendKvExpiry::Preserve,
            )
            .await
            .is_err()
        );

        set_kv(
            &pool,
            game_id,
            scope,
            "overflow",
            &json!(1e308),
            PuzzleBackendKvExpiry::Preserve,
        )
        .await
        .expect("finite number set should complete");
        assert!(
            increment_kv(
                &pool,
                game_id,
                scope,
                "overflow",
                1e308,
                PuzzleBackendKvExpiry::Preserve,
            )
            .await
            .is_err()
        );
    }
}
