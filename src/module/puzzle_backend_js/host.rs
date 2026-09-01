use std::{
    cell::RefCell,
    future::Future,
    sync::{Arc, Mutex},
};

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use tokio::runtime::Handle;

use crate::{
    AppState,
    db::{asset, puzzle_backend},
    error::RbInternalError,
    module::sync::PuzzleBackendEventSync,
};

use super::{RuntimeContext, protocol::*};

const DEFAULT_MAX_ASSET_READ_BYTES: u64 = 5 * 1024 * 1024;
const MAX_KV_TTL_MS: i64 = 365 * 24 * 60 * 60 * 1000;
const MAX_CONSOLE_ENTRIES: usize = 100;
const MAX_CONSOLE_ENTRY_BYTES: usize = 4 * 1024;
const MAX_CONSOLE_TOTAL_BYTES: usize = 64 * 1024;
const MAX_BACKEND_EVENTS: usize = 16;
const MAX_BACKEND_EVENT_PAYLOAD_BYTES: usize = 32 * 1024;

thread_local! {
    static JUDGE_CONN: RefCell<Option<*mut PgConnection>> = const { RefCell::new(None) };
    static TOKIO_HANDLE: RefCell<Option<Handle>> = const { RefCell::new(None) };
}

pub(super) struct JudgeConnGuard;

impl Drop for JudgeConnGuard {
    fn drop(&mut self) {
        JUDGE_CONN.with(|slot| *slot.borrow_mut() = None);
    }
}

pub(super) struct TokioHandleGuard;

impl Drop for TokioHandleGuard {
    fn drop(&mut self) {
        TOKIO_HANDLE.with(|slot| *slot.borrow_mut() = None);
    }
}

pub(super) fn set_judge_conn(conn: &mut PgConnection) -> JudgeConnGuard {
    JUDGE_CONN.with(|slot| *slot.borrow_mut() = Some(conn as *mut PgConnection));
    JudgeConnGuard
}

pub(super) fn set_tokio_handle(handle: Handle) -> TokioHandleGuard {
    TOKIO_HANDLE.with(|slot| *slot.borrow_mut() = Some(handle));
    TokioHandleGuard
}

pub(super) fn block_on_db<T>(
    future: impl Future<Output = Result<T, RbInternalError>>,
) -> Result<T, RbInternalError> {
    let handle = TOKIO_HANDLE.with(|slot| slot.borrow().clone());
    if let Some(handle) = handle {
        return handle.block_on(future);
    }
    futures::executor::block_on(future)
}

fn with_judge_conn<T>(
    f: impl FnOnce(&mut PgConnection) -> Result<T, RbInternalError>,
) -> Option<Result<T, RbInternalError>> {
    JUDGE_CONN.with(|slot| {
        let ptr = *slot.borrow();
        ptr.map(|ptr| {
            // SAFETY: the guard is scoped to the borrowed connection's lifetime. Host calls are
            // synchronous and execute on the same thread, so no overlapping mutable borrow exists.
            f(unsafe { &mut *ptr })
        })
    })
}

fn in_transactional_judge() -> bool {
    JUDGE_CONN.with(|slot| slot.borrow().is_some())
}

pub(super) trait HostBridge: Send + Sync {
    fn call(&self, request: HostRequest) -> Result<HostValue, HostError>;
}

#[derive(Clone, Serialize)]
struct BackendConsoleEntry {
    level: &'static str,
    message: String,
}

#[derive(Default)]
struct BackendConsoleCapture {
    entries: Vec<BackendConsoleEntry>,
    bytes: usize,
    truncated: bool,
}

impl BackendConsoleCapture {
    fn push(&mut self, level: HostConsoleLevel, message: String) {
        if self.truncated
            || self.entries.len() >= MAX_CONSOLE_ENTRIES
            || self.bytes >= MAX_CONSOLE_TOTAL_BYTES
        {
            self.truncated = true;
            return;
        }

        let remaining = MAX_CONSOLE_TOTAL_BYTES - self.bytes;
        let max_bytes = remaining.min(MAX_CONSOLE_ENTRY_BYTES);
        let (message, entry_truncated) = truncate_utf8(message, max_bytes);
        self.bytes += message.len();
        self.entries.push(BackendConsoleEntry {
            level: level.as_str(),
            message,
        });
        if entry_truncated {
            self.truncated = true;
        }
    }
}

fn truncate_utf8(mut value: String, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value, false);
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    (value, true)
}

struct EmittedPuzzleBackendEvent {
    event: String,
    payload: Value,
}

pub(super) struct HostCaptureReport {
    pub console: Value,
    pub console_truncated: bool,
    pub events: Result<Vec<PuzzleBackendEventSync>, HostError>,
}

pub(super) struct HostDispatcher {
    app: AppState,
    runtime: RuntimeContext,
    console: Mutex<BackendConsoleCapture>,
    events: Mutex<Vec<EmittedPuzzleBackendEvent>>,
    max_asset_read_bytes: u64,
}

impl HostDispatcher {
    pub fn new(app: AppState, runtime: RuntimeContext) -> Self {
        Self {
            app,
            runtime,
            console: Mutex::new(BackendConsoleCapture::default()),
            events: Mutex::new(Vec::new()),
            max_asset_read_bytes: DEFAULT_MAX_ASSET_READ_BYTES,
        }
    }

    pub fn capture_report(&self, success: bool, function_name: &str) -> HostCaptureReport {
        let (console, console_truncated) = match self.console.lock() {
            Ok(capture) => (
                serde_json::to_value(&capture.entries).unwrap_or_else(|_| Value::Array(vec![])),
                capture.truncated,
            ),
            Err(_) => (Value::Array(vec![]), true),
        };

        let events = if !success {
            Ok(vec![])
        } else {
            self.events
                .lock()
                .map_err(|_| {
                    HostError::new(
                        HostErrorKind::Unavailable,
                        "puzzle backend event capture is unavailable",
                    )
                })
                .map(|mut events| {
                    events
                        .drain(..)
                        .map(|event| PuzzleBackendEventSync {
                            puzzle_id: self.runtime.puzzle_id,
                            user_id: self.runtime.user_id,
                            user_nickname: self.runtime.user_nickname.clone(),
                            event: event.event,
                            payload: event.payload,
                            source_type: match self.runtime.method.as_str() {
                                "JUDGE" => "judge",
                                "HINT_PURCHASE" => "hintPurchase",
                                _ => "api",
                            },
                            function: function_name.to_string(),
                        })
                        .collect()
                })
        };

        HostCaptureReport {
            console,
            console_truncated,
            events,
        }
    }

    fn deadline(&self) -> Result<(), HostError> {
        if self.runtime.started_at.elapsed() <= self.runtime.timeout {
            Ok(())
        } else {
            Err(HostError::new(
                HostErrorKind::Timeout,
                "backend function execution timed out",
            ))
        }
    }

    fn db_error(error: impl ToString) -> HostError {
        HostError::internal(error.to_string())
    }

    fn scope(scope: HostScope) -> Result<puzzle_backend::BackendScope, HostError> {
        match scope {
            HostScope::Global => Ok(puzzle_backend::BackendScope::Global),
            HostScope::Team { team_id } if team_id > 0 => {
                Ok(puzzle_backend::BackendScope::Team { team_id })
            }
            HostScope::Puzzle { puzzle_id } if puzzle_id > 0 => {
                Ok(puzzle_backend::BackendScope::Puzzle { puzzle_id })
            }
            HostScope::TeamPuzzle { team_id, puzzle_id } if team_id > 0 && puzzle_id > 0 => {
                Ok(puzzle_backend::BackendScope::TeamPuzzle { team_id, puzzle_id })
            }
            _ => Err(HostError::invalid("scope contains an invalid id")),
        }
    }

    fn expiry(expiry: HostKvExpiry) -> Result<puzzle_backend::PuzzleBackendKvExpiry, HostError> {
        Ok(match expiry {
            HostKvExpiry::Preserve => puzzle_backend::PuzzleBackendKvExpiry::Preserve,
            HostKvExpiry::Permanent => puzzle_backend::PuzzleBackendKvExpiry::Permanent,
            HostKvExpiry::Ttl { ttl_ms } if (1..=MAX_KV_TTL_MS).contains(&ttl_ms) => {
                puzzle_backend::PuzzleBackendKvExpiry::Ttl(ttl_ms)
            }
            HostKvExpiry::Ttl { .. } => {
                return Err(HostError::invalid("KV expiry TTL is out of range"));
            }
        })
    }

    fn kv_entry_json(entry: puzzle_backend::PuzzleBackendKvValue) -> Value {
        json!({
            "value": entry.value,
            "version": entry.version.to_string(),
            "expiresAt": entry.expires_at.map(|value| {
                crate::serde_helpers::format_offset_datetime(&value)
            }),
        })
    }

    fn kv_mutation_json(result: puzzle_backend::PuzzleBackendKvMutation) -> Value {
        json!({
            "applied": result.applied,
            "entry": result.entry.map(Self::kv_entry_json),
            "serverTime": crate::serde_helpers::format_offset_datetime(&result.server_time),
        })
    }

    fn validate_store_name(name: &str, message: &str) -> Result<(), HostError> {
        if name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(HostError::invalid(message));
        }
        Ok(())
    }

    fn value_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
        let mut current = value;
        for part in path.split('.') {
            current = current.get(part)?;
        }
        Some(current)
    }

    fn index_entries(
        value: &Value,
        schema: &HostStoreSchema,
    ) -> Result<Vec<puzzle_backend::PuzzleStoreIndexEntry>, HostError> {
        let mut entries = Vec::with_capacity(schema.indexes.len());
        for (key, kind) in &schema.indexes {
            Self::validate_store_name(key, "$store index field name is invalid")?;
            let kind = kind.as_str().ok_or_else(|| {
                HostError::invalid("$store index type must be string, number, or boolean")
            })?;
            let Some(index_value) = Self::value_path(value, key) else {
                continue;
            };
            if index_value.is_null() {
                continue;
            }
            let value = match kind {
                "string" => puzzle_backend::PuzzleStoreIndexValue::Text(
                    index_value
                        .as_str()
                        .ok_or_else(|| {
                            HostError::invalid("$store indexed string field must be a string")
                        })?
                        .to_string(),
                ),
                "number" => puzzle_backend::PuzzleStoreIndexValue::Number(
                    index_value
                        .as_f64()
                        .filter(|value| value.is_finite())
                        .ok_or_else(|| {
                            HostError::invalid(
                                "$store indexed number field must be a finite number",
                            )
                        })?,
                ),
                "boolean" => {
                    puzzle_backend::PuzzleStoreIndexValue::Bool(index_value.as_bool().ok_or_else(
                        || HostError::invalid("$store indexed boolean field must be a boolean"),
                    )?)
                }
                _ => {
                    return Err(HostError::invalid(
                        "$store index type must be string, number, or boolean",
                    ));
                }
            };
            entries.push(puzzle_backend::PuzzleStoreIndexEntry {
                key: key.clone(),
                value,
            });
        }
        Ok(entries)
    }

    fn store_list_options(
        input: &HostStoreListOptions,
        schema: &HostStoreSchema,
    ) -> Result<puzzle_backend::PuzzleStoreListOptions, HostError> {
        let mut filters = puzzle_backend::PuzzleStoreEqFilters::empty();
        for (key, raw_filter) in &input.where_ {
            Self::validate_store_name(key, "$store filter field name is invalid")?;
            let kind = schema
                .indexes
                .get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    HostError::invalid(format!("$store filter field `{key}` is not indexed"))
                })?;
            let eq_value = raw_filter
                .as_object()
                .and_then(|object| object.get("eq"))
                .unwrap_or(raw_filter);
            match kind {
                "string" => filters.text.push((
                    key.clone(),
                    eq_value
                        .as_str()
                        .ok_or_else(|| {
                            HostError::invalid("$store string filter value must be a string")
                        })?
                        .to_string(),
                )),
                "number" => filters.number.push((
                    key.clone(),
                    eq_value
                        .as_f64()
                        .filter(|value| value.is_finite())
                        .ok_or_else(|| {
                            HostError::invalid("$store number filter value must be a finite number")
                        })?,
                )),
                "boolean" => filters.bool_.push((
                    key.clone(),
                    eq_value.as_bool().ok_or_else(|| {
                        HostError::invalid("$store boolean filter value must be a boolean")
                    })?,
                )),
                _ => {
                    return Err(HostError::invalid(
                        "$store index type must be string, number, or boolean",
                    ));
                }
            }
        }
        let cursor = input.cursor.as_ref().and_then(|value| match value {
            Value::Number(number) => number.as_i64(),
            Value::String(value) => value.parse::<i64>().ok(),
            _ => None,
        });
        Ok(puzzle_backend::PuzzleStoreListOptions {
            filters,
            cursor,
            limit: input.limit.unwrap_or(50).clamp(1, 100),
            descending: !matches!(input.order.as_deref(), Some("asc")),
        })
    }

    fn ensure_currency_team(
        &self,
        conn: Option<&mut PgConnection>,
        team_id: i32,
    ) -> Result<(), HostError> {
        let valid = match conn {
            Some(conn) => block_on_db(puzzle_backend::ensure_scope_in_game_conn(
                conn,
                self.runtime.game_id,
                puzzle_backend::BackendScope::Team { team_id },
            )),
            None => block_on_db(puzzle_backend::ensure_scope_in_game(
                &self.app.db,
                self.runtime.game_id,
                puzzle_backend::BackendScope::Team { team_id },
            )),
        }
        .map_err(Self::db_error)?;
        if valid {
            Ok(())
        } else {
            Err(HostError::invalid(
                "$game.currency team does not belong to current game",
            ))
        }
    }

    fn backend_currency_json(
        currency: Option<crate::db::team::RbCurrencyShowData>,
    ) -> Result<Value, HostError> {
        serde_json::to_value(currency.map(crate::db::team::PuzzleBackendCurrencyShowData::from))
            .map_err(Self::db_error)
    }

    fn backend_currencies_json(
        currencies: Vec<crate::db::team::RbCurrencyShowData>,
    ) -> Result<Value, HostError> {
        serde_json::to_value(
            currencies
                .into_iter()
                .map(crate::db::team::PuzzleBackendCurrencyShowData::from)
                .collect::<Vec<_>>(),
        )
        .map_err(Self::db_error)
    }

    fn currency_query(
        &self,
        team_id: i32,
        check_team: bool,
        currency: Option<HostCurrencyRef>,
    ) -> Result<Value, HostError> {
        self.deadline()?;
        if let Some(result) = with_judge_conn(|conn| {
            if check_team {
                self.ensure_currency_team(Some(&mut *conn), team_id)
                    .map_err(|error| RbInternalError::Other(error.message))?;
            }
            match &currency {
                Some(HostCurrencyRef::Id(currency_id)) => {
                    let row = block_on_db(crate::db::team::get_currency_info_one_all_conn(
                        conn,
                        team_id,
                        *currency_id,
                    ))?;
                    Self::backend_currency_json(row)
                        .map_err(|error| RbInternalError::Other(error.message))
                }
                Some(HostCurrencyRef::Slug(slug)) => {
                    let row =
                        block_on_db(crate::db::team::get_currency_info_one_by_slug_all_conn(
                            conn,
                            team_id,
                            self.runtime.game_id,
                            slug,
                        ))?;
                    Self::backend_currency_json(row)
                        .map_err(|error| RbInternalError::Other(error.message))
                }
                None => {
                    let rows =
                        block_on_db(crate::db::team::get_currency_info_all_conn(conn, team_id))?;
                    Self::backend_currencies_json(rows)
                        .map_err(|error| RbInternalError::Other(error.message))
                }
            }
        }) {
            return result.map_err(Self::db_error);
        }
        if check_team {
            self.ensure_currency_team(None, team_id)?;
        }
        match currency {
            Some(HostCurrencyRef::Id(currency_id)) => {
                let row = block_on_db(crate::db::team::get_currency_info_one_all(
                    &self.app.db,
                    team_id,
                    currency_id,
                ))
                .map_err(Self::db_error)?;
                Self::backend_currency_json(row)
            }
            Some(HostCurrencyRef::Slug(slug)) => {
                let row = block_on_db(crate::db::team::get_currency_info_one_by_slug_all(
                    &self.app.db,
                    team_id,
                    self.runtime.game_id,
                    &slug,
                ))
                .map_err(Self::db_error)?;
                Self::backend_currency_json(row)
            }
            None => {
                let rows = block_on_db(crate::db::team::get_currency_info_all(
                    &self.app.db,
                    team_id,
                ))
                .map_err(Self::db_error)?;
                Self::backend_currencies_json(rows)
            }
        }
    }

    fn currency_change(
        &self,
        team_id: i32,
        check_team: bool,
        currency: HostCurrencyRef,
        amount: String,
        reason: Option<String>,
        add: bool,
    ) -> Result<Value, HostError> {
        self.deadline()?;
        let amount = amount
            .parse::<i64>()
            .map_err(|_| HostError::invalid("invalid currency amount"))?;
        let event_context = crate::db::team::CurrencyEventContext {
            puzzle_id: Some(self.runtime.puzzle_id),
            puzzle_title: Some(&self.runtime.puzzle_title),
            reason: reason.as_deref(),
        };
        if add {
            if let Some(result) = with_judge_conn(|conn| {
                if check_team {
                    self.ensure_currency_team(Some(&mut *conn), team_id)
                        .map_err(|error| RbInternalError::Other(error.message))?;
                }
                match &currency {
                    HostCurrencyRef::Id(id) => block_on_db(crate::db::team::add_currency_conn(
                        conn,
                        team_id,
                        *id,
                        amount,
                        Some(event_context),
                    )),
                    HostCurrencyRef::Slug(slug) => {
                        block_on_db(crate::db::team::add_currency_by_slug_conn(
                            conn,
                            team_id,
                            self.runtime.game_id,
                            slug,
                            amount,
                            Some(event_context),
                        ))
                    }
                }
            }) {
                return result
                    .map(|value| value.map(Value::from).unwrap_or(Value::Null))
                    .map_err(Self::db_error);
            }
            if check_team {
                self.ensure_currency_team(None, team_id)?;
            }
            let updated = match &currency {
                HostCurrencyRef::Id(id) => block_on_db(crate::db::team::add_currency(
                    &self.app.db,
                    team_id,
                    *id,
                    amount,
                    Some(event_context),
                )),
                HostCurrencyRef::Slug(slug) => block_on_db(crate::db::team::add_currency_by_slug(
                    &self.app.db,
                    team_id,
                    self.runtime.game_id,
                    slug,
                    amount,
                    Some(event_context),
                )),
            }
            .map_err(Self::db_error)?;
            return Ok(updated.map(Value::from).unwrap_or(Value::Null));
        }

        if let Some(result) = with_judge_conn(|conn| {
            if check_team {
                self.ensure_currency_team(Some(&mut *conn), team_id)
                    .map_err(|error| RbInternalError::Other(error.message))?;
            }
            match &currency {
                HostCurrencyRef::Id(id) => block_on_db(crate::db::team::cost_currency_conn(
                    conn,
                    team_id,
                    *id,
                    amount,
                    Some(event_context),
                )),
                HostCurrencyRef::Slug(slug) => {
                    block_on_db(crate::db::team::cost_currency_by_slug_conn(
                        conn,
                        team_id,
                        self.runtime.game_id,
                        slug,
                        amount,
                        Some(event_context),
                    ))
                }
            }
        }) {
            return result.map(Value::Bool).map_err(Self::db_error);
        }
        if check_team {
            self.ensure_currency_team(None, team_id)?;
        }
        let updated = match &currency {
            HostCurrencyRef::Id(id) => block_on_db(crate::db::team::cost_currency(
                &self.app.db,
                team_id,
                *id,
                amount,
                Some(event_context),
            )),
            HostCurrencyRef::Slug(slug) => block_on_db(crate::db::team::cost_currency_by_slug(
                &self.app.db,
                team_id,
                self.runtime.game_id,
                slug,
                amount,
                Some(event_context),
            )),
        }
        .map_err(Self::db_error)?;
        Ok(Value::Bool(updated))
    }

    fn currency_update(
        &self,
        team_id: i32,
        check_team: bool,
        currency: HostCurrencyRef,
        options: HostCurrencyUpdate,
        reason: Option<String>,
    ) -> Result<Value, HostError> {
        self.deadline()?;
        let HostCurrencyUpdate {
            amount,
            team_growth,
            hidden,
        } = options;
        let options = crate::db::team::UpdateCurrencyOptions {
            amount: amount
                .map(|value| value.parse::<i64>())
                .transpose()
                .map_err(|_| HostError::invalid("invalid currency.update amount"))?,
            team_growth: team_growth
                .map(|value| value.parse::<i64>())
                .transpose()
                .map_err(|_| HostError::invalid("invalid currency.update teamGrowth"))?,
            hidden,
        };
        let event_context = crate::db::team::CurrencyEventContext {
            puzzle_id: Some(self.runtime.puzzle_id),
            puzzle_title: Some(&self.runtime.puzzle_title),
            reason: reason.as_deref(),
        };
        if let Some(result) = with_judge_conn(|conn| {
            if check_team {
                self.ensure_currency_team(Some(&mut *conn), team_id)
                    .map_err(|error| RbInternalError::Other(error.message))?;
            }
            match &currency {
                HostCurrencyRef::Id(id) => block_on_db(crate::db::team::update_currency_conn(
                    conn,
                    team_id,
                    *id,
                    options,
                    Some(event_context),
                )),
                HostCurrencyRef::Slug(slug) => {
                    block_on_db(crate::db::team::update_currency_by_slug_conn(
                        conn,
                        team_id,
                        self.runtime.game_id,
                        slug,
                        options,
                        Some(event_context),
                    ))
                }
            }
        }) {
            return Self::backend_currency_json(result.map_err(Self::db_error)?);
        }
        if check_team {
            self.ensure_currency_team(None, team_id)?;
        }
        let updated = match currency {
            HostCurrencyRef::Id(id) => block_on_db(crate::db::team::update_currency(
                &self.app.db,
                team_id,
                id,
                options,
                Some(event_context),
            )),
            HostCurrencyRef::Slug(slug) => block_on_db(crate::db::team::update_currency_by_slug(
                &self.app.db,
                team_id,
                self.runtime.game_id,
                &slug,
                options,
                Some(event_context),
            )),
        }
        .map_err(Self::db_error)?;
        Self::backend_currency_json(updated)
    }

    fn readable_asset_file(
        &self,
        object_key: &str,
        relative_path: &str,
    ) -> Result<asset::RbAssetReadableFile, HostError> {
        let file = if let Some(result) = with_judge_conn(|conn| {
            block_on_db(asset::get_readable_file_by_object_key_conn(
                conn,
                self.runtime.game_id,
                self.runtime.puzzle_id,
                object_key,
                relative_path,
            ))
        }) {
            result
        } else {
            block_on_db(asset::get_readable_file_by_object_key(
                &self.app.db,
                self.runtime.game_id,
                self.runtime.puzzle_id,
                object_key,
                relative_path,
            ))
        }
        .map_err(Self::db_error)?
        .ok_or_else(|| {
            HostError::new(
                HostErrorKind::NotFound,
                "$asset file not found or not readable",
            )
        })?;
        Ok(file)
    }

    fn validate_asset_path(path: &str, message: &'static str) -> Result<(), HostError> {
        if path.is_empty() || path.len() > 1024 || path.contains('\0') {
            return Err(HostError::invalid(message));
        }
        Ok(())
    }

    fn read_asset_bytes(
        &self,
        object_key: &str,
        relative_path: &str,
    ) -> Result<Vec<u8>, HostError> {
        let file = self.readable_asset_file(object_key, relative_path)?;
        if file.size < 0 || file.size as u64 > self.max_asset_read_bytes {
            return Err(HostError::new(
                HostErrorKind::LimitExceeded,
                "$asset file is too large",
            ));
        }
        if self.app.storage.is_database(&file.backend) {
            if let Some(content) = self.app.storage.cached_database_asset(&file.sha256) {
                return Ok(content.as_ref().to_vec());
            }
            let content = if let Some(result) = with_judge_conn(|conn| {
                block_on_db(asset::get_file_blob_conn(
                    conn,
                    file.group_id,
                    &file.relative_path,
                ))
            }) {
                result
            } else {
                block_on_db(asset::get_file_blob(
                    &self.app.db,
                    file.group_id,
                    &file.relative_path,
                ))
            }
            .map_err(Self::db_error)?
            .ok_or_else(|| {
                HostError::new(HostErrorKind::NotFound, "$asset file content not found")
            })?;
            if content.len() as i64 != file.size
                || format!("{:x}", Sha256::digest(&content)) != file.sha256
            {
                return Err(HostError::internal(
                    "$asset file content integrity check failed",
                ));
            }
            let content: Arc<[u8]> = content.into();
            self.app
                .storage
                .cache_database_asset(&file.sha256, content.clone());
            return Ok(content.as_ref().to_vec());
        }

        let Some(local) = self.app.storage.local(&file.backend) else {
            return Err(HostError::new(
                HostErrorKind::Unavailable,
                "$asset backend resources are not backend-readable",
            ));
        };
        block_on_db(local.read_object_file_limited(
            &file.object_key,
            &file.relative_path,
            self.max_asset_read_bytes,
        ))
        .map_err(Self::db_error)
    }

    fn submission_action(value: &Value) -> Result<crate::model::game::RbJudgeAction, HostError> {
        if let Some(action) = value.as_str() {
            return Ok(match action {
                "fail" => crate::model::game::RbJudgeAction::Fail,
                "correct" => crate::model::game::RbJudgeAction::Correct,
                "milestone" => crate::model::game::RbJudgeAction::Milestone,
                "startGame" => crate::model::game::RbJudgeAction::StartGame,
                "easterEgg" => crate::model::game::RbJudgeAction::EasterEgg,
                "finishGame" => crate::model::game::RbJudgeAction::FinishGame,
                _ => crate::model::game::RbJudgeAction::Error,
            });
        }
        if let Some(action) = value.as_i64() {
            return Ok(match action as i16 {
                -2 => crate::model::game::RbJudgeAction::Error,
                -1 => crate::model::game::RbJudgeAction::Pending,
                0 => crate::model::game::RbJudgeAction::Fail,
                1 => crate::model::game::RbJudgeAction::Correct,
                2 => crate::model::game::RbJudgeAction::Milestone,
                3 => crate::model::game::RbJudgeAction::StartGame,
                4 => crate::model::game::RbJudgeAction::EasterEgg,
                5 => crate::model::game::RbJudgeAction::FinishGame,
                _ => crate::model::game::RbJudgeAction::Invalid,
            });
        }
        Err(HostError::invalid("submission action must be a string"))
    }

    fn submission_input(
        value: &Value,
    ) -> Result<crate::db::puzzle::BackendSubmissionInput, HostError> {
        let object = value
            .as_object()
            .ok_or_else(|| HostError::invalid("submission must be an object"))?;
        let user_answer = object
            .get("userAnswer")
            .and_then(Value::as_str)
            .ok_or_else(|| HostError::invalid("submission.userAnswer is required"))?
            .to_string();
        Ok(crate::db::puzzle::BackendSubmissionInput {
            user_answer,
            norm_answer: object
                .get("normAnswer")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            action: object
                .get("action")
                .map(Self::submission_action)
                .transpose()?
                .unwrap_or(crate::model::game::RbJudgeAction::Correct),
            result: object
                .get("result")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            real_answer: object
                .get("realAnswer")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            ignored: object
                .get("ignored")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    fn capture_event(
        events: &Mutex<Vec<EmittedPuzzleBackendEvent>>,
        event: String,
        payload: Value,
    ) -> Result<(), HostError> {
        let mut chars = event.chars();
        let valid = matches!(chars.next(), Some(first) if first.is_ascii_alphabetic())
            && event.len() <= 64
            && chars.all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
            });
        if !valid {
            return Err(HostError::invalid(
                "$this.event.emit event name must start with a letter and contain at most 64 letters, numbers, _, -, or .",
            ));
        }
        let payload_bytes = serde_json::to_vec(&payload).map_err(Self::db_error)?.len();
        if payload_bytes > MAX_BACKEND_EVENT_PAYLOAD_BYTES {
            return Err(HostError::new(
                HostErrorKind::LimitExceeded,
                format!("$this.event.emit payload exceeds {MAX_BACKEND_EVENT_PAYLOAD_BYTES} bytes"),
            ));
        }
        let mut events = events.lock().map_err(|_| {
            HostError::new(
                HostErrorKind::Unavailable,
                "$this.event.emit capture is unavailable",
            )
        })?;
        if events.len() >= MAX_BACKEND_EVENTS {
            return Err(HostError::new(
                HostErrorKind::LimitExceeded,
                format!(
                    "$this.event.emit allows at most {MAX_BACKEND_EVENTS} events per execution"
                ),
            ));
        }
        events.push(EmittedPuzzleBackendEvent { event, payload });
        Ok(())
    }

    fn emit_event(&self, event: String, payload: Value) -> Result<(), HostError> {
        Self::capture_event(&self.events, event, payload)
    }
}

impl HostBridge for HostDispatcher {
    fn call(&self, request: HostRequest) -> Result<HostValue, HostError> {
        if request.protocol_version != HOST_PROTOCOL_VERSION {
            return Err(HostError::new(
                HostErrorKind::Unavailable,
                format!(
                    "unsupported puzzle backend host protocol version {}",
                    request.protocol_version
                ),
            ));
        }
        let json = match request.call {
            HostCall::KvGet { scope, key } => {
                self.deadline()?;
                let scope = Self::scope(scope)?;
                if let Some(result) = with_judge_conn(|conn| {
                    block_on_db(puzzle_backend::get_kv_conn(
                        conn,
                        self.runtime.game_id,
                        scope,
                        &key,
                    ))
                }) {
                    result
                } else {
                    block_on_db(puzzle_backend::get_kv(
                        &self.app.db,
                        self.runtime.game_id,
                        scope,
                        &key,
                    ))
                }
                .map_err(Self::db_error)?
                .unwrap_or(Value::Null)
            }
            HostCall::KvGetEntry { scope, key } => {
                self.deadline()?;
                let scope = Self::scope(scope)?;
                let entry = if let Some(result) = with_judge_conn(|conn| {
                    block_on_db(puzzle_backend::get_kv_entry_conn(
                        conn,
                        self.runtime.game_id,
                        scope,
                        &key,
                    ))
                }) {
                    result
                } else {
                    block_on_db(puzzle_backend::get_kv_entry(
                        &self.app.db,
                        self.runtime.game_id,
                        scope,
                        &key,
                    ))
                }
                .map_err(Self::db_error)?;
                entry.map(Self::kv_entry_json).unwrap_or(Value::Null)
            }
            HostCall::KvSet {
                scope,
                key,
                value,
                expiry,
            } => {
                self.deadline()?;
                let scope = Self::scope(scope)?;
                let expiry = Self::expiry(expiry)?;
                let mutation = if let Some(result) = with_judge_conn(|conn| {
                    block_on_db(puzzle_backend::set_kv_conn(
                        conn,
                        self.runtime.game_id,
                        scope,
                        &key,
                        &value,
                        expiry,
                    ))
                }) {
                    result
                } else {
                    block_on_db(puzzle_backend::set_kv(
                        &self.app.db,
                        self.runtime.game_id,
                        scope,
                        &key,
                        &value,
                        expiry,
                    ))
                }
                .map_err(Self::db_error)?;
                Self::kv_mutation_json(mutation)
            }
            HostCall::KvIncrement {
                scope,
                key,
                amount,
                expiry,
            } => {
                self.deadline()?;
                if !amount.is_finite() {
                    return Err(HostError::invalid("kv increment amount must be finite"));
                }
                let scope = Self::scope(scope)?;
                let expiry = Self::expiry(expiry)?;
                let mutation = if let Some(result) = with_judge_conn(|conn| {
                    block_on_db(puzzle_backend::increment_kv_conn(
                        conn,
                        self.runtime.game_id,
                        scope,
                        &key,
                        amount,
                        expiry,
                    ))
                }) {
                    result
                } else {
                    block_on_db(puzzle_backend::increment_kv(
                        &self.app.db,
                        self.runtime.game_id,
                        scope,
                        &key,
                        amount,
                        expiry,
                    ))
                }
                .map_err(Self::db_error)?;
                Self::kv_mutation_json(mutation)
            }
            HostCall::KvSetIfAbsent {
                scope,
                key,
                value,
                expiry,
            } => {
                self.deadline()?;
                let scope = Self::scope(scope)?;
                let ttl_ms = match expiry {
                    HostKvExpiry::Ttl { ttl_ms } if (1..=MAX_KV_TTL_MS).contains(&ttl_ms) => {
                        Some(ttl_ms)
                    }
                    HostKvExpiry::Ttl { .. } => {
                        return Err(HostError::invalid("KV expiry TTL is out of range"));
                    }
                    HostKvExpiry::Permanent => None,
                    HostKvExpiry::Preserve => {
                        return Err(HostError::invalid("setIfAbsent cannot preserve expiry"));
                    }
                };
                let mutation = if let Some(result) = with_judge_conn(|conn| {
                    block_on_db(puzzle_backend::set_kv_if_absent_conn(
                        conn,
                        self.runtime.game_id,
                        scope,
                        &key,
                        &value,
                        ttl_ms,
                    ))
                }) {
                    result
                } else {
                    block_on_db(puzzle_backend::set_kv_if_absent(
                        &self.app.db,
                        self.runtime.game_id,
                        scope,
                        &key,
                        &value,
                        ttl_ms,
                    ))
                }
                .map_err(Self::db_error)?;
                Self::kv_mutation_json(mutation)
            }
            HostCall::KvCompareAndSet {
                scope,
                key,
                expected_version,
                value,
                expiry,
            } => {
                self.deadline()?;
                let scope = Self::scope(scope)?;
                let expected_version = expected_version
                    .parse::<i64>()
                    .ok()
                    .filter(|value| *value > 0)
                    .ok_or_else(|| HostError::invalid("invalid expected KV version"))?;
                let expiry = Self::expiry(expiry)?;
                let mutation = if let Some(result) = with_judge_conn(|conn| {
                    block_on_db(puzzle_backend::compare_and_set_kv_conn(
                        conn,
                        self.runtime.game_id,
                        scope,
                        &key,
                        expected_version,
                        &value,
                        expiry,
                    ))
                }) {
                    result
                } else {
                    block_on_db(puzzle_backend::compare_and_set_kv(
                        &self.app.db,
                        self.runtime.game_id,
                        scope,
                        &key,
                        expected_version,
                        &value,
                        expiry,
                    ))
                }
                .map_err(Self::db_error)?;
                Self::kv_mutation_json(mutation)
            }
            HostCall::KvDelete { scope, key } => {
                self.deadline()?;
                let scope = Self::scope(scope)?;
                let deleted = if let Some(result) = with_judge_conn(|conn| {
                    block_on_db(puzzle_backend::delete_kv_conn(
                        conn,
                        self.runtime.game_id,
                        scope,
                        &key,
                    ))
                }) {
                    result
                } else {
                    block_on_db(puzzle_backend::delete_kv(
                        &self.app.db,
                        self.runtime.game_id,
                        scope,
                        &key,
                    ))
                }
                .map_err(Self::db_error)?;
                Value::Bool(deleted)
            }
            HostCall::StoreInsert {
                scope,
                collection,
                schema,
                value,
            } => {
                Self::validate_store_name(
                    &collection,
                    "$store collection name must be 1-64 chars using letters, numbers, _, -, or .",
                )?;
                if !value.is_object() {
                    return Err(HostError::invalid(
                        "$store.collection(...).insert requires an object value",
                    ));
                }
                let indexes = Self::index_entries(&value, &schema)?;
                self.deadline()?;
                let scope = Self::scope(scope)?;
                let doc = if let Some(result) = with_judge_conn(|conn| {
                    block_on_db(puzzle_backend::insert_store_doc_conn(
                        conn,
                        self.runtime.game_id,
                        scope,
                        &collection,
                        self.runtime.user_id,
                        &value,
                        &indexes,
                    ))
                }) {
                    result
                } else {
                    block_on_db(puzzle_backend::insert_store_doc(
                        &self.app.db,
                        self.runtime.game_id,
                        scope,
                        &collection,
                        self.runtime.user_id,
                        &value,
                        &indexes,
                    ))
                }
                .map_err(Self::db_error)?;
                serde_json::to_value(doc).map_err(Self::db_error)?
            }
            HostCall::StoreGet {
                scope,
                collection,
                doc_id,
            } => {
                Self::validate_store_name(
                    &collection,
                    "$store collection name must be 1-64 chars using letters, numbers, _, -, or .",
                )?;
                let doc_id = doc_id
                    .parse::<i64>()
                    .map_err(|_| HostError::invalid("invalid store document id"))?;
                self.deadline()?;
                let scope = Self::scope(scope)?;
                let doc = if let Some(result) = with_judge_conn(|conn| {
                    block_on_db(puzzle_backend::get_store_doc_conn(
                        conn,
                        self.runtime.game_id,
                        scope,
                        &collection,
                        doc_id,
                    ))
                }) {
                    result
                } else {
                    block_on_db(puzzle_backend::get_store_doc(
                        &self.app.db,
                        self.runtime.game_id,
                        scope,
                        &collection,
                        doc_id,
                    ))
                }
                .map_err(Self::db_error)?;
                serde_json::to_value(doc).map_err(Self::db_error)?
            }
            HostCall::StoreList {
                scope,
                collection,
                schema,
                options,
            } => {
                Self::validate_store_name(
                    &collection,
                    "$store collection name must be 1-64 chars using letters, numbers, _, -, or .",
                )?;
                let options = Self::store_list_options(&options, &schema)?;
                self.deadline()?;
                let scope = Self::scope(scope)?;
                let docs = if let Some(result) = with_judge_conn(|conn| {
                    block_on_db(puzzle_backend::list_store_docs_conn(
                        conn,
                        self.runtime.game_id,
                        scope,
                        &collection,
                        &options,
                    ))
                }) {
                    result
                } else {
                    block_on_db(puzzle_backend::list_store_docs(
                        &self.app.db,
                        self.runtime.game_id,
                        scope,
                        &collection,
                        &options,
                    ))
                }
                .map_err(Self::db_error)?;
                serde_json::to_value(docs).map_err(Self::db_error)?
            }
            HostCall::CurrencyQuery {
                team_id,
                check_team,
                currency,
            } => self.currency_query(team_id, check_team, currency)?,
            HostCall::CurrencyCost {
                team_id,
                check_team,
                currency,
                amount,
                reason,
            } => self.currency_change(team_id, check_team, currency, amount, reason, false)?,
            HostCall::CurrencyAdd {
                team_id,
                check_team,
                currency,
                amount,
                reason,
            } => self.currency_change(team_id, check_team, currency, amount, reason, true)?,
            HostCall::CurrencyUpdate {
                team_id,
                check_team,
                currency,
                options,
                reason,
            } => self.currency_update(team_id, check_team, currency, options, reason)?,
            HostCall::AssetList { object_key } => {
                let files = block_on_db(asset::list_readable_files_by_object_key(
                    &self.app.db,
                    self.runtime.game_id,
                    self.runtime.puzzle_id,
                    &object_key,
                ))
                .map_err(Self::db_error)?;
                serde_json::to_value(files).map_err(Self::db_error)?
            }
            HostCall::AssetReadText {
                object_key,
                relative_path,
            } => {
                Self::validate_asset_path(
                    &relative_path,
                    "$puzzle.assets.readText requires a relative path",
                )?;
                let bytes = self.read_asset_bytes(&object_key, &relative_path)?;
                let text = String::from_utf8(bytes).map_err(|_| {
                    HostError::invalid("$puzzle.assets.readText requires UTF-8 content")
                })?;
                Value::String(text)
            }
            HostCall::AssetReadJson {
                object_key,
                relative_path,
            } => {
                Self::validate_asset_path(
                    &relative_path,
                    "$puzzle.assets.readJson requires a relative path",
                )?;
                let bytes = self.read_asset_bytes(&object_key, &relative_path)?;
                serde_json::from_slice(&bytes).map_err(|error| {
                    HostError::invalid(format!("$puzzle.assets.readJson failed: {error}"))
                })?
            }
            HostCall::AssetReadBytes {
                object_key,
                relative_path,
            } => {
                Self::validate_asset_path(
                    &relative_path,
                    "$puzzle.assets.readBytes requires a relative path",
                )?;
                serde_json::to_value(self.read_asset_bytes(&object_key, &relative_path)?)
                    .map_err(Self::db_error)?
            }
            HostCall::SubmissionAdd { submission } => {
                if in_transactional_judge() {
                    return Err(HostError::new(
                        HostErrorKind::Forbidden,
                        "$this.submission.add is not available in judge functions",
                    ));
                }
                let input = Self::submission_input(&submission)?;
                let row = block_on_db(crate::db::puzzle::add_backend_submission(
                    &self.app,
                    self.runtime.team_id,
                    self.runtime.user_id,
                    self.runtime.puzzle_id,
                    &input,
                ))
                .map_err(Self::db_error)?;
                serde_json::to_value(row).map_err(Self::db_error)?
            }
            HostCall::PuzzleSolve { submission } => {
                if in_transactional_judge() {
                    return Err(HostError::new(
                        HostErrorKind::Forbidden,
                        "$this.solve is not available in judge functions",
                    ));
                }
                let submission = serde_json::from_value(submission)
                    .map_err(|error| HostError::invalid(format!("invalid submission: {error}")))?;
                let solved = block_on_db(crate::db::puzzle::solve_backend_puzzle_with_submission(
                    &self.app,
                    self.runtime.team_id,
                    self.runtime.user_id,
                    self.runtime.puzzle_id,
                    &submission,
                ))
                .map_err(Self::db_error)?;
                Value::Bool(solved)
            }
            HostCall::EventEmit { event, payload } => {
                self.emit_event(event, payload)?;
                return Ok(HostValue::Undefined);
            }
            HostCall::ConsoleWrite { level, message } => {
                self.console
                    .lock()
                    .map_err(|_| {
                        HostError::new(
                            HostErrorKind::Unavailable,
                            "puzzle backend console capture is unavailable",
                        )
                    })?
                    .push(level, message);
                return Ok(HostValue::Undefined);
            }
        };

        Ok(HostValue::Json(json))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_capture_truncates_at_utf8_boundary() {
        let mut capture = BackendConsoleCapture::default();
        capture.push(HostConsoleLevel::Log, "测".repeat(MAX_CONSOLE_ENTRY_BYTES));
        assert_eq!(capture.entries.len(), 1);
        assert!(capture.entries[0].message.len() <= MAX_CONSOLE_ENTRY_BYTES);
        assert!(
            capture.entries[0]
                .message
                .is_char_boundary(capture.entries[0].message.len())
        );
        assert!(capture.truncated);
    }

    #[test]
    fn console_capture_enforces_entry_and_total_limits() {
        let mut entries = BackendConsoleCapture::default();
        for index in 0..=MAX_CONSOLE_ENTRIES {
            entries.push(HostConsoleLevel::Info, index.to_string());
        }
        assert_eq!(entries.entries.len(), MAX_CONSOLE_ENTRIES);
        assert!(entries.truncated);

        let mut bytes = BackendConsoleCapture::default();
        for _ in 0..=MAX_CONSOLE_TOTAL_BYTES / MAX_CONSOLE_ENTRY_BYTES {
            bytes.push(HostConsoleLevel::Warn, "x".repeat(MAX_CONSOLE_ENTRY_BYTES));
        }
        assert_eq!(bytes.bytes, MAX_CONSOLE_TOTAL_BYTES);
        assert!(bytes.truncated);
    }

    #[test]
    fn event_names_are_validated() {
        for name in ["level_completed", "level.completed", "Level-2"] {
            assert!(
                HostDispatcher::capture_event(&Mutex::new(vec![]), name.to_string(), Value::Null)
                    .is_ok()
            );
        }
        for name in ["", "2level", ".level", "level completed"] {
            assert!(
                HostDispatcher::capture_event(&Mutex::new(vec![]), name.to_string(), Value::Null)
                    .is_err()
            );
        }
    }

    #[test]
    fn event_capture_preserves_order_and_enforces_limits() {
        let events = Mutex::new(vec![]);
        for index in 0..MAX_BACKEND_EVENTS {
            HostDispatcher::capture_event(&events, format!("event_{index}"), Value::from(index))
                .expect("event should be captured");
        }
        let captured = events.lock().expect("events lock");
        assert_eq!(captured.len(), MAX_BACKEND_EVENTS);
        assert_eq!(captured[0].event, "event_0");
        assert_eq!(
            captured[MAX_BACKEND_EVENTS - 1].payload,
            Value::from(MAX_BACKEND_EVENTS - 1)
        );
        drop(captured);
        assert!(
            HostDispatcher::capture_event(&events, "overflow".to_string(), Value::Null).is_err()
        );

        let oversized = Value::String("x".repeat(MAX_BACKEND_EVENT_PAYLOAD_BYTES));
        assert!(
            HostDispatcher::capture_event(&Mutex::new(vec![]), "large".to_string(), oversized)
                .is_err()
        );
    }

    #[test]
    fn kv_expiry_enforces_ttl_bounds() {
        assert!(HostDispatcher::expiry(HostKvExpiry::Preserve).is_ok());
        assert!(HostDispatcher::expiry(HostKvExpiry::Permanent).is_ok());
        assert!(HostDispatcher::expiry(HostKvExpiry::Ttl { ttl_ms: 30_000 }).is_ok());
        for ttl_ms in [0, MAX_KV_TTL_MS + 1] {
            assert!(HostDispatcher::expiry(HostKvExpiry::Ttl { ttl_ms }).is_err());
        }
    }

    #[test]
    fn kv_entry_versions_are_exposed_as_strings() {
        let entry = HostDispatcher::kv_entry_json(puzzle_backend::PuzzleBackendKvValue {
            value: json!({ "ready": true }),
            version: 9_007_199_254_740_993,
            expires_at: None,
        });
        assert_eq!(entry["version"], "9007199254740993");
        assert_eq!(entry["value"], json!({ "ready": true }));
        assert_eq!(entry["expiresAt"], Value::Null);
    }

    #[test]
    fn submission_input_uses_public_action_and_result_names() {
        let input = HostDispatcher::submission_input(&json!({
            "userAnswer": "answer",
            "action": "startGame",
            "result": "close",
        }))
        .expect("submission should parse");
        assert_eq!(i16::from(input.action), 3);
        assert_eq!(input.result.as_deref(), Some("close"));
    }
}
