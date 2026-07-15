use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use boa_engine::{
    Context, JsError, JsNativeError, JsString, JsValue, Module, NativeFunction, Source,
    builtins::promise::PromiseState,
    js_string,
    object::{JsObject, ObjectInitializer},
    property::Attribute,
};
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use tokio::runtime::Handle;

use crate::{
    AppState, DbPool,
    db::{asset, puzzle_backend},
    error::RbInternalError,
    module::{storage::StorageManager, sync::PuzzleBackendEventSync},
};

#[derive(Clone)]
pub struct RuntimeContext {
    pub game_id: i32,
    pub method: String,
    pub puzzle_id: i32,
    pub team_id: i32,
    pub user_id: i32,
    pub api_name: String,
    pub submission_id: Option<i32>,
    pub hint_id: Option<i32>,
    pub query: Value,
    pub body: Value,
    pub puzzle_title: String,
    pub user_nickname: String,
    pub team_name: String,
    pub started_at: Instant,
    pub timeout: Duration,
}

#[derive(Clone)]
pub struct JudgeRuntimeContext {
    pub puzzle_id: i32,
    pub game_id: i32,
    pub puzzle_title: String,
    pub team_id: i32,
    pub team_name: String,
    pub user_id: i32,
    pub user_nickname: String,
    pub user_answer: String,
    pub norm_answer: String,
    pub submission: crate::db::puzzle::BackendSubmissionShowData,
}

#[derive(Clone)]
pub struct HintPurchaseRuntimeContext {
    pub puzzle_id: i32,
    pub game_id: i32,
    pub puzzle_title: String,
    pub team_id: i32,
    pub team_name: String,
    pub user_id: i32,
    pub user_nickname: String,
    pub hint_id: i32,
    pub hint_title: String,
    pub cost_id: Option<i32>,
    pub cost_amount: i64,
    pub currency: Value,
}

#[derive(Clone)]
pub struct RuntimeServices {
    pub app: AppState,
    pub asset_runtime: AssetRuntime,
    event_capture: Arc<Mutex<Vec<EmittedPuzzleBackendEvent>>>,
}

pub struct BackendExecution<T> {
    pub value: T,
    pub events: Vec<PuzzleBackendEventSync>,
}

struct EmittedPuzzleBackendEvent {
    event: String,
    payload: Value,
}

#[derive(Clone)]
pub struct AssetRuntime {
    pub db: DbPool,
    pub storage: StorageManager,
    pub max_read_bytes: u64,
}

const DEFAULT_MAX_ASSET_READ_BYTES: u64 = 5 * 1024 * 1024;
const DEFAULT_BACKEND_FUNCTION_TIMEOUT: Duration = Duration::from_secs(5);
const JUDGE_EXECUTION_TIMEOUT: Duration = Duration::from_millis(500);
const JUDGE_LOOP_ITERATION_LIMIT: u64 = 50_000;
const MAX_CONSOLE_ENTRIES: usize = 100;
const MAX_CONSOLE_ENTRY_BYTES: usize = 4 * 1024;
const MAX_CONSOLE_TOTAL_BYTES: usize = 64 * 1024;
const MAX_BACKEND_EVENTS: usize = 16;
const MAX_BACKEND_EVENT_PAYLOAD_BYTES: usize = 32 * 1024;

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
    fn push(&mut self, level: &'static str, message: String) {
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
        self.entries.push(BackendConsoleEntry { level, message });
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

fn console_function(
    capture: Rc<RefCell<BackendConsoleCapture>>,
    level: &'static str,
) -> NativeFunction {
    unsafe {
        NativeFunction::from_closure_with_captures(
            move |_, args, _, context| {
                let message = args
                    .iter()
                    .map(|value| {
                        value
                            .to_string(context)
                            .map(|value| value.to_std_string_escaped())
                            .unwrap_or_else(|error| format!("<unprintable: {error}>"))
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                capture.borrow_mut().push(level, message);
                Ok(JsValue::undefined())
            },
            (),
        )
    }
}

fn register_console(
    context: &mut Context,
    capture: Rc<RefCell<BackendConsoleCapture>>,
) -> Result<(), RbInternalError> {
    let console = ObjectInitializer::new(context)
        .function(
            console_function(capture.clone(), "debug"),
            js_string!("debug"),
            1,
        )
        .function(
            console_function(capture.clone(), "log"),
            js_string!("log"),
            1,
        )
        .function(
            console_function(capture.clone(), "info"),
            js_string!("info"),
            1,
        )
        .function(
            console_function(capture.clone(), "warn"),
            js_string!("warn"),
            1,
        )
        .function(console_function(capture, "error"), js_string!("error"), 1)
        .build();
    context
        .register_global_property(js_string!("console"), console, Attribute::all())
        .map_err(|error| internal_err(error.to_string()))?;
    Ok(())
}

thread_local! {
    static RUNTIME_CONTEXT: RefCell<Option<RuntimeContext>> = const { RefCell::new(None) };
    static JUDGE_CONN: RefCell<Option<*mut PgConnection>> = const { RefCell::new(None) };
    static TOKIO_HANDLE: RefCell<Option<Handle>> = const { RefCell::new(None) };
}

struct RuntimeContextGuard;

impl Drop for RuntimeContextGuard {
    fn drop(&mut self) {
        RUNTIME_CONTEXT.with(|slot| {
            *slot.borrow_mut() = None;
        });
    }
}

struct JudgeConnGuard;

impl Drop for JudgeConnGuard {
    fn drop(&mut self) {
        JUDGE_CONN.with(|slot| {
            *slot.borrow_mut() = None;
        });
    }
}

struct TokioHandleGuard;

impl Drop for TokioHandleGuard {
    fn drop(&mut self) {
        TOKIO_HANDLE.with(|slot| {
            *slot.borrow_mut() = None;
        });
    }
}

fn set_tokio_handle(handle: Handle) -> TokioHandleGuard {
    TOKIO_HANDLE.with(|slot| {
        *slot.borrow_mut() = Some(handle);
    });
    TokioHandleGuard
}

fn with_runtime_context<T>(f: impl FnOnce(&RuntimeContext) -> T) -> Result<T, RbInternalError> {
    RUNTIME_CONTEXT.with(|slot| {
        let borrowed = slot.borrow();
        let Some(ctx) = borrowed.as_ref() else {
            return Err(RbInternalError::Other(
                "missing runtime context".to_string(),
            ));
        };
        Ok(f(ctx))
    })
}

fn with_judge_conn<T>(
    f: impl FnOnce(&mut PgConnection) -> Result<T, RbInternalError>,
) -> Option<Result<T, RbInternalError>> {
    JUDGE_CONN.with(|slot| {
        let ptr = *slot.borrow();
        ptr.map(|ptr| {
            // SAFETY: JUDGE_CONN is only set by execute_judge_conn while the borrowed
            // PgConnection is alive, and Boa executes JS on the same thread synchronously.
            let conn = unsafe { &mut *ptr };
            f(conn)
        })
    })
}

fn in_transactional_judge() -> bool {
    JUDGE_CONN.with(|slot| slot.borrow().is_some())
}

fn check_runtime_deadline() -> Result<(), RbInternalError> {
    with_runtime_context(|ctx| ctx.started_at.elapsed() <= ctx.timeout).and_then(|ok| {
        if ok {
            Ok(())
        } else {
            Err(internal_err("backend function execution timed out"))
        }
    })
}

fn js_err(msg: impl Into<String>) -> boa_engine::JsError {
    JsNativeError::typ().with_message(msg.into()).into()
}

fn internal_err(msg: impl Into<String>) -> RbInternalError {
    RbInternalError::Other(msg.into())
}

fn js_number_to_i64(value: f64, message: &str) -> Result<i64, JsError> {
    if !value.is_finite()
        || value.fract() != 0.0
        || value < i64::MIN as f64
        || value > i64::MAX as f64
    {
        return Err(js_err(message));
    }
    Ok(value as i64)
}

fn js_number_to_i32_id(value: f64, message: &str) -> Result<i32, JsError> {
    if !value.is_finite() || value.fract() != 0.0 || value < 1.0 || value > i32::MAX as f64 {
        return Err(js_err(message));
    }
    Ok(value as i32)
}

fn json_to_js(value: &Value, context: &mut Context) -> Result<JsValue, RbInternalError> {
    JsValue::from_json(value, context).map_err(|e| internal_err(e.to_string()))
}

fn js_to_json(value: &JsValue, context: &mut Context) -> Result<Value, RbInternalError> {
    value
        .to_json(context)
        .map_err(|e| internal_err(e.to_string()))?
        .ok_or_else(|| internal_err("value is undefined"))
}

fn js_to_json_optional(
    value: &JsValue,
    context: &mut Context,
) -> Result<Option<Value>, RbInternalError> {
    value
        .to_json(context)
        .map_err(|e| internal_err(e.to_string()))
}

fn js_string_arg(value: Option<JsString>, message: &str) -> Result<String, JsError> {
    value
        .map(|v| v.to_std_string_escaped())
        .ok_or_else(|| js_err(message))
}

const MAX_KV_TTL_MS: i64 = 365 * 24 * 60 * 60 * 1000;

fn kv_expiry_arg(
    value: Option<&JsValue>,
    context: &mut Context,
    omitted: puzzle_backend::PuzzleBackendKvExpiry,
    label: &str,
) -> Result<puzzle_backend::PuzzleBackendKvExpiry, JsError> {
    let Some(value) = value.filter(|value| !value.is_undefined() && !value.is_null()) else {
        return Ok(omitted);
    };
    let value = js_to_json(value, context).map_err(|e| js_err(e.to_string()))?;
    let options = value
        .as_object()
        .ok_or_else(|| js_err(format!("{label} options must be an object")))?;
    let Some(ttl_ms) = options.get("ttl") else {
        return Ok(omitted);
    };
    if ttl_ms.is_null() {
        return Ok(puzzle_backend::PuzzleBackendKvExpiry::Permanent);
    }
    let ttl_ms = ttl_ms.as_i64().ok_or_else(|| {
        js_err(format!(
            "{label} options.ttl must be an integer between 1 and {MAX_KV_TTL_MS} milliseconds"
        ))
    })?;
    if !(1..=MAX_KV_TTL_MS).contains(&ttl_ms) {
        return Err(js_err(format!(
            "{label} options.ttl must be an integer between 1 and {MAX_KV_TTL_MS} milliseconds"
        )));
    }
    Ok(puzzle_backend::PuzzleBackendKvExpiry::Ttl(ttl_ms))
}

fn kv_increment_amount_arg(value: Option<&JsValue>, label: &str) -> Result<f64, JsError> {
    let Some(value) = value.filter(|value| !value.is_undefined()) else {
        return Ok(1.0);
    };
    value
        .as_number()
        .filter(|value| value.is_finite())
        .ok_or_else(|| js_err(format!("{label} amount must be a finite number")))
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
        "entry": result.entry.map(kv_entry_json),
        "serverTime": crate::serde_helpers::format_offset_datetime(&result.server_time),
    })
}

enum CurrencyRef {
    Id(i32),
    Slug(String),
}

fn currency_ref_arg(value: Option<&JsValue>, message: &str) -> Result<CurrencyRef, JsError> {
    let Some(value) = value else {
        return Err(js_err(message));
    };

    if let Some(id) = value.as_number() {
        return Ok(CurrencyRef::Id(js_number_to_i32_id(
            id,
            "currency id must be a positive integer in i32 range",
        )?));
    }

    if let Some(slug) = value.as_string() {
        return Ok(CurrencyRef::Slug(slug.to_std_string_escaped()));
    }

    Err(js_err(message))
}

fn optional_reason_arg(value: Option<&JsValue>, message: &str) -> Result<Option<String>, JsError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() || value.is_undefined() {
        return Ok(None);
    }
    let Some(reason) = value.as_string() else {
        return Err(js_err(message));
    };
    Ok(Some(reason.to_std_string_escaped()))
}

fn block_on_db<T>(
    future: impl std::future::Future<Output = Result<T, RbInternalError>>,
) -> Result<T, RbInternalError> {
    let handle = TOKIO_HANDLE.with(|slot| slot.borrow().clone());
    if let Some(handle) = handle {
        return handle.block_on(future);
    }

    futures::executor::block_on(future)
}

fn block_on_io<T>(
    future: impl std::future::Future<Output = Result<T, RbInternalError>>,
) -> Result<T, RbInternalError> {
    block_on_db(future)
}

fn js_asset_path_arg(value: Option<JsString>, message: &str) -> Result<String, JsError> {
    let path = js_string_arg(value, message)?;
    if path.is_empty() || path.len() > 1024 || path.contains('\0') {
        return Err(js_err(message));
    }
    Ok(path)
}

fn readable_asset_file(
    asset_runtime: &AssetRuntime,
    object_key: &str,
    relative_path: &str,
) -> Result<asset::RbAssetReadableFile, JsError> {
    let runtime = with_runtime_context(|ctx| ctx.clone()).map_err(|e| js_err(e.to_string()))?;
    let file = if let Some(result) = with_judge_conn(|conn| {
        block_on_db(asset::get_readable_file_by_object_key_conn(
            conn,
            runtime.game_id,
            runtime.puzzle_id,
            object_key,
            relative_path,
        ))
    }) {
        result
    } else {
        block_on_db(asset::get_readable_file_by_object_key(
            &asset_runtime.db,
            runtime.game_id,
            runtime.puzzle_id,
            object_key,
            relative_path,
        ))
    }
    .map_err(|e| js_err(e.to_string()))?
    .ok_or_else(|| js_err("$asset file not found or not readable"))?;
    Ok(file)
}

fn read_asset_bytes(
    asset_runtime: &AssetRuntime,
    object_key: &str,
    relative_path: &str,
) -> Result<Vec<u8>, JsError> {
    let file = readable_asset_file(asset_runtime, object_key, relative_path)?;
    if file.size < 0 || file.size as u64 > asset_runtime.max_read_bytes {
        return Err(js_err("$asset file is too large"));
    }
    if asset_runtime.storage.is_database(&file.backend) {
        if let Some(content) = asset_runtime.storage.cached_database_asset(&file.sha256) {
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
                &asset_runtime.db,
                file.group_id,
                &file.relative_path,
            ))
        }
        .map_err(|e| js_err(e.to_string()))?
        .ok_or_else(|| js_err("$asset file content not found"))?;
        if content.len() as i64 != file.size
            || format!("{:x}", Sha256::digest(&content)) != file.sha256
        {
            return Err(js_err("$asset file content integrity check failed"));
        }
        let content: std::sync::Arc<[u8]> = content.into();
        asset_runtime
            .storage
            .cache_database_asset(&file.sha256, content.clone());
        return Ok(content.as_ref().to_vec());
    }

    let Some(local) = asset_runtime.storage.local(&file.backend) else {
        return Err(js_err("$asset backend resources are not backend-readable"));
    };
    block_on_io(local.read_object_file_limited(
        &file.object_key,
        &file.relative_path,
        asset_runtime.max_read_bytes,
    ))
    .map_err(|e| js_err(e.to_string()))
}

fn build_ctx_arg(context: &mut Context) -> Result<JsValue, RbInternalError> {
    let runtime = with_runtime_context(|ctx| ctx.clone())?;
    let request_query = json_to_js(&runtime.query, context)?;
    let request_body = json_to_js(&runtime.body, context)?;
    let request = ObjectInitializer::new(context)
        .property(
            js_string!("method"),
            JsValue::from(JsString::from(runtime.method.clone())),
            Attribute::all(),
        )
        .property(js_string!("query"), request_query, Attribute::all())
        .property(js_string!("body"), request_body, Attribute::all())
        .build();

    let puzzle = ObjectInitializer::new(context)
        .property(js_string!("id"), runtime.puzzle_id, Attribute::all())
        .property(js_string!("gameId"), runtime.game_id, Attribute::all())
        .property(
            js_string!("title"),
            JsValue::from(JsString::from(runtime.puzzle_title)),
            Attribute::all(),
        )
        .build();

    let user = ObjectInitializer::new(context)
        .property(js_string!("id"), runtime.user_id, Attribute::all())
        .property(
            js_string!("nickname"),
            JsValue::from(JsString::from(runtime.user_nickname)),
            Attribute::all(),
        )
        .build();

    let team = ObjectInitializer::new(context)
        .property(js_string!("id"), runtime.team_id, Attribute::all())
        .property(
            js_string!("name"),
            JsValue::from(JsString::from(runtime.team_name)),
            Attribute::all(),
        )
        .build();

    let ctx = ObjectInitializer::new(context)
        .property(js_string!("request"), request, Attribute::all())
        .property(js_string!("puzzle"), puzzle, Attribute::all())
        .property(js_string!("user"), user, Attribute::all())
        .property(js_string!("team"), team, Attribute::all())
        .property(
            js_string!("apiName"),
            JsValue::from(JsString::from(runtime.api_name)),
            Attribute::all(),
        )
        .build();

    Ok(ctx.into())
}

fn build_judge_ctx_arg(
    context: &mut Context,
    runtime: &JudgeRuntimeContext,
) -> Result<JsValue, RbInternalError> {
    let request = ObjectInitializer::new(context)
        .property(
            js_string!("userAnswer"),
            JsValue::from(JsString::from(runtime.user_answer.clone())),
            Attribute::all(),
        )
        .property(
            js_string!("normAnswer"),
            JsValue::from(JsString::from(runtime.norm_answer.clone())),
            Attribute::all(),
        )
        .build();

    let puzzle = ObjectInitializer::new(context)
        .property(js_string!("id"), runtime.puzzle_id, Attribute::all())
        .property(js_string!("gameId"), runtime.game_id, Attribute::all())
        .property(
            js_string!("title"),
            JsValue::from(JsString::from(runtime.puzzle_title.clone())),
            Attribute::all(),
        )
        .build();

    let team = ObjectInitializer::new(context)
        .property(js_string!("id"), runtime.team_id, Attribute::all())
        .property(
            js_string!("name"),
            JsValue::from(JsString::from(runtime.team_name.clone())),
            Attribute::all(),
        )
        .build();

    let user = ObjectInitializer::new(context)
        .property(js_string!("id"), runtime.user_id, Attribute::all())
        .property(
            js_string!("nickname"),
            JsValue::from(JsString::from(runtime.user_nickname.clone())),
            Attribute::all(),
        )
        .build();

    let submission = ObjectInitializer::new(context)
        .property(js_string!("id"), runtime.submission.id, Attribute::all())
        .property(
            js_string!("createdAt"),
            JsValue::from(JsString::from(
                crate::serde_helpers::format_offset_datetime(&runtime.submission.ctime_at),
            )),
            Attribute::all(),
        )
        .build();

    let ctx = ObjectInitializer::new(context)
        .property(js_string!("request"), request, Attribute::all())
        .property(js_string!("puzzle"), puzzle, Attribute::all())
        .property(js_string!("team"), team, Attribute::all())
        .property(js_string!("user"), user, Attribute::all())
        .property(js_string!("submission"), submission, Attribute::all())
        .property(
            js_string!("apiName"),
            JsValue::from(JsString::from("judge")),
            Attribute::all(),
        )
        .build();

    Ok(ctx.into())
}

fn build_hint_purchase_ctx_arg(
    context: &mut Context,
    runtime: &HintPurchaseRuntimeContext,
    function_name: &str,
) -> Result<JsValue, RbInternalError> {
    let puzzle = ObjectInitializer::new(context)
        .property(js_string!("id"), runtime.puzzle_id, Attribute::all())
        .property(js_string!("gameId"), runtime.game_id, Attribute::all())
        .property(
            js_string!("title"),
            JsValue::from(JsString::from(runtime.puzzle_title.clone())),
            Attribute::all(),
        )
        .build();

    let team = ObjectInitializer::new(context)
        .property(js_string!("id"), runtime.team_id, Attribute::all())
        .property(
            js_string!("name"),
            JsValue::from(JsString::from(runtime.team_name.clone())),
            Attribute::all(),
        )
        .build();

    let user = ObjectInitializer::new(context)
        .property(js_string!("id"), runtime.user_id, Attribute::all())
        .property(
            js_string!("nickname"),
            JsValue::from(JsString::from(runtime.user_nickname.clone())),
            Attribute::all(),
        )
        .build();

    let hint = ObjectInitializer::new(context)
        .property(js_string!("id"), runtime.hint_id, Attribute::all())
        .property(
            js_string!("title"),
            JsValue::from(JsString::from(runtime.hint_title.clone())),
            Attribute::all(),
        )
        .property(
            js_string!("costId"),
            runtime
                .cost_id
                .map(JsValue::from)
                .unwrap_or_else(JsValue::null),
            Attribute::all(),
        )
        .property(
            js_string!("costAmount"),
            runtime.cost_amount,
            Attribute::all(),
        )
        .build();

    let currency = json_to_js(&runtime.currency, context)?;
    let purchase = ObjectInitializer::new(context)
        .property(js_string!("currency"), currency, Attribute::all())
        .build();

    let ctx = ObjectInitializer::new(context)
        .property(js_string!("puzzle"), puzzle, Attribute::all())
        .property(js_string!("team"), team, Attribute::all())
        .property(js_string!("user"), user, Attribute::all())
        .property(js_string!("hint"), hint, Attribute::all())
        .property(js_string!("purchase"), purchase, Attribute::all())
        .property(
            js_string!("apiName"),
            JsValue::from(JsString::from(function_name)),
            Attribute::all(),
        )
        .build();

    Ok(ctx.into())
}

fn validate_store_name(name: &str, message: &str) -> Result<(), JsError> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(js_err(message));
    }
    Ok(())
}

fn object_arg(
    value: Option<&JsValue>,
    message: &str,
    context: &mut Context,
) -> Result<Value, JsError> {
    let value = value.cloned().unwrap_or_else(JsValue::null);
    js_to_json(&value, context)
        .map_err(|e| js_err(e.to_string()))
        .and_then(|value| {
            if value.is_object() {
                Ok(value)
            } else {
                Err(js_err(message))
            }
        })
}

fn value_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

fn index_entries_from_value(
    value: &Value,
    index_schema: &Map<String, Value>,
) -> Result<Vec<puzzle_backend::PuzzleStoreIndexEntry>, JsError> {
    let mut entries = Vec::with_capacity(index_schema.len());
    for (key, kind) in index_schema {
        validate_store_name(key, "$store index field name is invalid")?;
        let kind = kind
            .as_str()
            .ok_or_else(|| js_err("$store index type must be string, number, or boolean"))?;
        let Some(index_value) = value_path(value, key) else {
            continue;
        };
        if index_value.is_null() {
            continue;
        }
        let value = match kind {
            "string" => puzzle_backend::PuzzleStoreIndexValue::Text(
                index_value
                    .as_str()
                    .ok_or_else(|| js_err("$store indexed string field must be a string"))?
                    .to_string(),
            ),
            "number" => puzzle_backend::PuzzleStoreIndexValue::Number(
                index_value
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .ok_or_else(|| js_err("$store indexed number field must be a finite number"))?,
            ),
            "boolean" => puzzle_backend::PuzzleStoreIndexValue::Bool(
                index_value
                    .as_bool()
                    .ok_or_else(|| js_err("$store indexed boolean field must be a boolean"))?,
            ),
            _ => {
                return Err(js_err(
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

fn filters_from_options(
    options: &Value,
    index_schema: &Map<String, Value>,
) -> Result<puzzle_backend::PuzzleStoreEqFilters, JsError> {
    let mut filters = puzzle_backend::PuzzleStoreEqFilters::empty();
    let Some(where_value) = options.get("where") else {
        return Ok(filters);
    };
    let where_object = where_value
        .as_object()
        .ok_or_else(|| js_err("$store list where must be an object"))?;
    for (key, raw_filter) in where_object {
        validate_store_name(key, "$store filter field name is invalid")?;
        let kind = index_schema
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| js_err(format!("$store filter field `{key}` is not indexed")))?;
        let eq_value = raw_filter
            .as_object()
            .and_then(|object| object.get("eq"))
            .unwrap_or(raw_filter);
        match kind {
            "string" => filters.text.push((
                key.clone(),
                eq_value
                    .as_str()
                    .ok_or_else(|| js_err("$store string filter value must be a string"))?
                    .to_string(),
            )),
            "number" => filters.number.push((
                key.clone(),
                eq_value
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .ok_or_else(|| js_err("$store number filter value must be a finite number"))?,
            )),
            "boolean" => filters.bool_.push((
                key.clone(),
                eq_value
                    .as_bool()
                    .ok_or_else(|| js_err("$store boolean filter value must be a boolean"))?,
            )),
            _ => {
                return Err(js_err(
                    "$store index type must be string, number, or boolean",
                ));
            }
        }
    }
    Ok(filters)
}

fn list_options_from_value(
    value: &Value,
    index_schema: &Map<String, Value>,
) -> Result<puzzle_backend::PuzzleStoreListOptions, JsError> {
    let filters = filters_from_options(value, index_schema)?;
    let limit = value
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(50)
        .clamp(1, 100);
    let cursor = value.get("cursor").and_then(|value| match value {
        Value::Number(number) => number.as_i64(),
        Value::String(value) => value.parse::<i64>().ok(),
        _ => None,
    });
    let descending = !matches!(value.get("order").and_then(Value::as_str), Some("asc"));
    Ok(puzzle_backend::PuzzleStoreListOptions {
        filters,
        cursor,
        limit,
        descending,
    })
}

fn submission_action_from_value(
    value: &Value,
) -> Result<crate::model::game::RbJudgeAction, JsError> {
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
    Err(js_err("submission action must be a string"))
}

fn backend_submission_input_from_value(
    value: &Value,
) -> Result<crate::db::puzzle::BackendSubmissionInput, JsError> {
    let Some(obj) = value.as_object() else {
        return Err(js_err("submission must be an object"));
    };
    let user_answer = obj
        .get("userAnswer")
        .and_then(Value::as_str)
        .ok_or_else(|| js_err("submission.userAnswer is required"))?
        .to_string();
    let norm_answer = obj
        .get("normAnswer")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let saction = obj
        .get("action")
        .map(submission_action_from_value)
        .transpose()?
        .unwrap_or(crate::model::game::RbJudgeAction::Correct);
    let sresult = obj
        .get("result")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let real_answer = obj
        .get("realAnswer")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let ignored = obj.get("ignored").and_then(Value::as_bool).unwrap_or(false);

    Ok(crate::db::puzzle::BackendSubmissionInput {
        user_answer,
        norm_answer,
        action: saction,
        result: sresult,
        real_answer,
        ignored,
    })
}

fn backend_submission_from_value(
    value: &Value,
) -> Result<crate::db::puzzle::BackendSubmissionShowData, JsError> {
    serde_json::from_value(value.clone()).map_err(|e| js_err(format!("invalid submission: {e}")))
}

#[derive(Clone, Copy)]
enum ScopeSelector {
    GameArg,
    Team,
    Puzzle,
    This,
}

#[derive(Clone, Copy)]
enum CurrencySelector {
    GameArg,
    Team,
}

fn scope_from_js(
    value: &JsValue,
    context: &mut Context,
) -> Result<puzzle_backend::BackendScope, JsError> {
    let value = js_to_json(value, context).map_err(|e| js_err(e.to_string()))?;
    let object = value
        .as_object()
        .ok_or_else(|| js_err("scope must be an object"))?;
    match object.get("type").and_then(Value::as_str) {
        Some("global") => Ok(puzzle_backend::BackendScope::Global),
        Some("team") => {
            let team_id = object
                .get("teamId")
                .and_then(Value::as_i64)
                .ok_or_else(|| js_err("team scope requires teamId"))?;
            Ok(puzzle_backend::BackendScope::Team {
                team_id: i32::try_from(team_id)
                    .ok()
                    .filter(|value| *value > 0)
                    .ok_or_else(|| js_err("teamId must be a positive integer in i32 range"))?,
            })
        }
        Some("puzzle") => {
            let puzzle_id = object
                .get("puzzleId")
                .and_then(Value::as_i64)
                .ok_or_else(|| js_err("puzzle scope requires puzzleId"))?;
            Ok(puzzle_backend::BackendScope::Puzzle {
                puzzle_id: i32::try_from(puzzle_id)
                    .ok()
                    .filter(|value| *value > 0)
                    .ok_or_else(|| js_err("puzzleId must be a positive integer in i32 range"))?,
            })
        }
        Some("teamPuzzle") => {
            let team_id = object
                .get("teamId")
                .and_then(Value::as_i64)
                .ok_or_else(|| js_err("teamPuzzle scope requires teamId"))?;
            let puzzle_id = object
                .get("puzzleId")
                .and_then(Value::as_i64)
                .ok_or_else(|| js_err("teamPuzzle scope requires puzzleId"))?;
            Ok(puzzle_backend::BackendScope::TeamPuzzle {
                team_id: i32::try_from(team_id)
                    .ok()
                    .filter(|value| *value > 0)
                    .ok_or_else(|| js_err("teamId must be a positive integer in i32 range"))?,
                puzzle_id: i32::try_from(puzzle_id)
                    .ok()
                    .filter(|value| *value > 0)
                    .ok_or_else(|| js_err("puzzleId must be a positive integer in i32 range"))?,
            })
        }
        _ => Err(js_err(
            "scope.type must be global, team, puzzle, or teamPuzzle",
        )),
    }
}

fn runtime_scope(
    selector: ScopeSelector,
    args: &[JsValue],
    context: &mut Context,
) -> Result<(puzzle_backend::BackendScope, usize), JsError> {
    let runtime = with_runtime_context(|ctx| ctx.clone()).map_err(|e| js_err(e.to_string()))?;
    match selector {
        ScopeSelector::GameArg => {
            let scope = args
                .first()
                .ok_or_else(|| js_err("$game scope argument is required"))
                .and_then(|value| scope_from_js(value, context))?;
            Ok((scope, 1))
        }
        ScopeSelector::Team => Ok((
            puzzle_backend::BackendScope::Team {
                team_id: runtime.team_id,
            },
            0,
        )),
        ScopeSelector::Puzzle => Ok((
            puzzle_backend::BackendScope::Puzzle {
                puzzle_id: runtime.puzzle_id,
            },
            0,
        )),
        ScopeSelector::This => Ok((
            puzzle_backend::BackendScope::TeamPuzzle {
                team_id: runtime.team_id,
                puzzle_id: runtime.puzzle_id,
            },
            0,
        )),
    }
}

fn runtime_currency_team(
    selector: CurrencySelector,
    args: &[JsValue],
) -> Result<(i32, usize, bool), JsError> {
    let runtime = with_runtime_context(|ctx| ctx.clone()).map_err(|e| js_err(e.to_string()))?;
    match selector {
        CurrencySelector::GameArg => {
            let team_id = args
                .first()
                .and_then(|value| value.as_number())
                .ok_or_else(|| js_err("$game.currency requires team id"))?;
            Ok((
                js_number_to_i32_id(
                    team_id,
                    "$game.currency team id must be a positive integer in i32 range",
                )?,
                1,
                true,
            ))
        }
        CurrencySelector::Team => Ok((runtime.team_id, 0, false)),
    }
}

fn require_currency_team_in_game(db: &DbPool, team_id: i32, game_id: i32) -> Result<(), JsError> {
    let valid = block_on_db(puzzle_backend::ensure_scope_in_game(
        db,
        game_id,
        puzzle_backend::BackendScope::Team { team_id },
    ))
    .map_err(|e| js_err(e.to_string()))?;
    if valid {
        Ok(())
    } else {
        Err(js_err(
            "$game.currency team does not belong to current game",
        ))
    }
}

fn require_currency_team_in_game_conn(
    conn: &mut PgConnection,
    team_id: i32,
    game_id: i32,
) -> Result<(), JsError> {
    let valid = block_on_db(puzzle_backend::ensure_scope_in_game_conn(
        conn,
        game_id,
        puzzle_backend::BackendScope::Team { team_id },
    ))
    .map_err(|e| js_err(e.to_string()))?;
    if valid {
        Ok(())
    } else {
        Err(js_err(
            "$game.currency team does not belong to current game",
        ))
    }
}

fn build_kv_object(
    context: &mut Context,
    db: DbPool,
    selector: ScopeSelector,
    label: &'static str,
) -> JsObject {
    let get_db = db.clone();
    let get_entry_db = db.clone();
    let set_db = db.clone();
    let increment_db = db.clone();
    let set_if_absent_db = db.clone();
    let compare_and_set_db = db.clone();
    let delete_db = db;
    ObjectInitializer::new(context)
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, context| {
                        let (scope, offset) = runtime_scope(selector, args, context)?;
                        let key = js_string_arg(
                            args.get(offset).and_then(|value| value.as_string()),
                            &format!("{label}.kv.get requires a key"),
                        )?;
                        let runtime = with_runtime_context(|ctx| ctx.clone())
                            .map_err(|e| js_err(e.to_string()))?;
                        check_runtime_deadline().map_err(|e| js_err(e.to_string()))?;
                        let value = if let Some(result) = with_judge_conn(|conn| {
                            block_on_db(puzzle_backend::get_kv_conn(
                                conn,
                                runtime.game_id,
                                scope,
                                &key,
                            ))
                        }) {
                            result
                        } else {
                            block_on_db(puzzle_backend::get_kv(
                                &get_db,
                                runtime.game_id,
                                scope,
                                &key,
                            ))
                        }
                        .map_err(|e| js_err(e.to_string()))?
                        .unwrap_or(Value::Null);
                        json_to_js(&value, context).map_err(|e| js_err(e.to_string()))
                    },
                    (),
                )
            },
            js_string!("get"),
            2,
        )
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, context| {
                        let (scope, offset) = runtime_scope(selector, args, context)?;
                        let key = js_string_arg(
                            args.get(offset).and_then(|value| value.as_string()),
                            &format!("{label}.kv.getEntry requires a key"),
                        )?;
                        let runtime = with_runtime_context(|ctx| ctx.clone())
                            .map_err(|e| js_err(e.to_string()))?;
                        check_runtime_deadline().map_err(|e| js_err(e.to_string()))?;
                        let entry = if let Some(result) = with_judge_conn(|conn| {
                            block_on_db(puzzle_backend::get_kv_entry_conn(
                                conn,
                                runtime.game_id,
                                scope,
                                &key,
                            ))
                        }) {
                            result
                        } else {
                            block_on_db(puzzle_backend::get_kv_entry(
                                &get_entry_db,
                                runtime.game_id,
                                scope,
                                &key,
                            ))
                        }
                        .map_err(|e| js_err(e.to_string()))?;
                        let value = entry.map(kv_entry_json).unwrap_or(Value::Null);
                        json_to_js(&value, context).map_err(|e| js_err(e.to_string()))
                    },
                    (),
                )
            },
            js_string!("getEntry"),
            2,
        )
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, context| {
                        let (scope, offset) = runtime_scope(selector, args, context)?;
                        let key = js_string_arg(
                            args.get(offset).and_then(|value| value.as_string()),
                            &format!("{label}.kv.set requires a key"),
                        )?;
                        let value = args.get(offset + 1).cloned().unwrap_or_else(JsValue::null);
                        let value =
                            js_to_json(&value, context).map_err(|e| js_err(e.to_string()))?;
                        let expiry = kv_expiry_arg(
                            args.get(offset + 2),
                            context,
                            puzzle_backend::PuzzleBackendKvExpiry::Preserve,
                            &format!("{label}.kv.set"),
                        )?;
                        let runtime = with_runtime_context(|ctx| ctx.clone())
                            .map_err(|e| js_err(e.to_string()))?;
                        check_runtime_deadline().map_err(|e| js_err(e.to_string()))?;
                        let value = if let Some(result) = with_judge_conn(|conn| {
                            block_on_db(puzzle_backend::set_kv_conn(
                                conn,
                                runtime.game_id,
                                scope,
                                &key,
                                &value,
                                expiry,
                            ))
                        }) {
                            result
                        } else {
                            block_on_db(puzzle_backend::set_kv(
                                &set_db,
                                runtime.game_id,
                                scope,
                                &key,
                                &value,
                                expiry,
                            ))
                        }
                        .map_err(|e| js_err(e.to_string()))?;
                        let value = kv_mutation_json(value);
                        json_to_js(&value, context).map_err(|e| js_err(e.to_string()))
                    },
                    (),
                )
            },
            js_string!("set"),
            4,
        )
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, context| {
                        let (scope, offset) = runtime_scope(selector, args, context)?;
                        let key = js_string_arg(
                            args.get(offset).and_then(|value| value.as_string()),
                            &format!("{label}.kv.increment requires a key"),
                        )?;
                        let amount = kv_increment_amount_arg(
                            args.get(offset + 1),
                            &format!("{label}.kv.increment"),
                        )?;
                        let expiry = kv_expiry_arg(
                            args.get(offset + 2),
                            context,
                            puzzle_backend::PuzzleBackendKvExpiry::Preserve,
                            &format!("{label}.kv.increment"),
                        )?;
                        let runtime = with_runtime_context(|ctx| ctx.clone())
                            .map_err(|e| js_err(e.to_string()))?;
                        check_runtime_deadline().map_err(|e| js_err(e.to_string()))?;
                        let result = if let Some(result) = with_judge_conn(|conn| {
                            block_on_db(puzzle_backend::increment_kv_conn(
                                conn,
                                runtime.game_id,
                                scope,
                                &key,
                                amount,
                                expiry,
                            ))
                        }) {
                            result
                        } else {
                            block_on_db(puzzle_backend::increment_kv(
                                &increment_db,
                                runtime.game_id,
                                scope,
                                &key,
                                amount,
                                expiry,
                            ))
                        }
                        .map_err(|e| js_err(e.to_string()))?;
                        let value = kv_mutation_json(result);
                        json_to_js(&value, context).map_err(|e| js_err(e.to_string()))
                    },
                    (),
                )
            },
            js_string!("increment"),
            4,
        )
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, context| {
                        let (scope, offset) = runtime_scope(selector, args, context)?;
                        let key = js_string_arg(
                            args.get(offset).and_then(|value| value.as_string()),
                            &format!("{label}.kv.setIfAbsent requires a key"),
                        )?;
                        let value = args.get(offset + 1).cloned().unwrap_or_else(JsValue::null);
                        let value =
                            js_to_json(&value, context).map_err(|e| js_err(e.to_string()))?;
                        let expiry = kv_expiry_arg(
                            args.get(offset + 2),
                            context,
                            puzzle_backend::PuzzleBackendKvExpiry::Permanent,
                            &format!("{label}.kv.setIfAbsent"),
                        )?;
                        let ttl_ms = match expiry {
                            puzzle_backend::PuzzleBackendKvExpiry::Ttl(value) => Some(value),
                            puzzle_backend::PuzzleBackendKvExpiry::Permanent => None,
                            puzzle_backend::PuzzleBackendKvExpiry::Preserve => unreachable!(),
                        };
                        let runtime = with_runtime_context(|ctx| ctx.clone())
                            .map_err(|e| js_err(e.to_string()))?;
                        check_runtime_deadline().map_err(|e| js_err(e.to_string()))?;
                        let result = if let Some(result) = with_judge_conn(|conn| {
                            block_on_db(puzzle_backend::set_kv_if_absent_conn(
                                conn,
                                runtime.game_id,
                                scope,
                                &key,
                                &value,
                                ttl_ms,
                            ))
                        }) {
                            result
                        } else {
                            block_on_db(puzzle_backend::set_kv_if_absent(
                                &set_if_absent_db,
                                runtime.game_id,
                                scope,
                                &key,
                                &value,
                                ttl_ms,
                            ))
                        }
                        .map_err(|e| js_err(e.to_string()))?;
                        let value = kv_mutation_json(result);
                        json_to_js(&value, context).map_err(|e| js_err(e.to_string()))
                    },
                    (),
                )
            },
            js_string!("setIfAbsent"),
            4,
        )
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, context| {
                        let (scope, offset) = runtime_scope(selector, args, context)?;
                        let key = js_string_arg(
                            args.get(offset).and_then(|value| value.as_string()),
                            &format!("{label}.kv.compareAndSet requires a key"),
                        )?;
                        let expected_version = js_string_arg(
                            args.get(offset + 1).and_then(|value| value.as_string()),
                            &format!(
                                "{label}.kv.compareAndSet requires an expected version string"
                            ),
                        )?
                        .parse::<i64>()
                        .ok()
                        .filter(|value| *value > 0)
                        .ok_or_else(|| {
                            js_err(format!(
                                "{label}.kv.compareAndSet expected version is invalid"
                            ))
                        })?;
                        let value = args.get(offset + 2).cloned().unwrap_or_else(JsValue::null);
                        let value =
                            js_to_json(&value, context).map_err(|e| js_err(e.to_string()))?;
                        let expiry = kv_expiry_arg(
                            args.get(offset + 3),
                            context,
                            puzzle_backend::PuzzleBackendKvExpiry::Preserve,
                            &format!("{label}.kv.compareAndSet"),
                        )?;
                        let runtime = with_runtime_context(|ctx| ctx.clone())
                            .map_err(|e| js_err(e.to_string()))?;
                        check_runtime_deadline().map_err(|e| js_err(e.to_string()))?;
                        let result = if let Some(result) = with_judge_conn(|conn| {
                            block_on_db(puzzle_backend::compare_and_set_kv_conn(
                                conn,
                                runtime.game_id,
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
                                &compare_and_set_db,
                                runtime.game_id,
                                scope,
                                &key,
                                expected_version,
                                &value,
                                expiry,
                            ))
                        }
                        .map_err(|e| js_err(e.to_string()))?;
                        let value = kv_mutation_json(result);
                        json_to_js(&value, context).map_err(|e| js_err(e.to_string()))
                    },
                    (),
                )
            },
            js_string!("compareAndSet"),
            5,
        )
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, _context| {
                        let (scope, offset) = runtime_scope(selector, args, _context)?;
                        let key = js_string_arg(
                            args.get(offset).and_then(|value| value.as_string()),
                            &format!("{label}.kv.delete requires a key"),
                        )?;
                        let runtime = with_runtime_context(|ctx| ctx.clone())
                            .map_err(|e| js_err(e.to_string()))?;
                        check_runtime_deadline().map_err(|e| js_err(e.to_string()))?;
                        let deleted = if let Some(result) = with_judge_conn(|conn| {
                            block_on_db(puzzle_backend::delete_kv_conn(
                                conn,
                                runtime.game_id,
                                scope,
                                &key,
                            ))
                        }) {
                            result
                        } else {
                            block_on_db(puzzle_backend::delete_kv(
                                &delete_db,
                                runtime.game_id,
                                scope,
                                &key,
                            ))
                        }
                        .map_err(|e| js_err(e.to_string()))?;
                        Ok(JsValue::from(deleted))
                    },
                    (),
                )
            },
            js_string!("delete"),
            2,
        )
        .build()
}

fn schema_indexes_arg(
    value: Option<&JsValue>,
    context: &mut Context,
) -> Result<Map<String, Value>, JsError> {
    let schema = value
        .map(|value| js_to_json(value, context))
        .transpose()
        .map_err(|e| js_err(e.to_string()))?
        .unwrap_or(Value::Null);
    Ok(schema
        .get("indexes")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default())
}

fn build_store_object(
    context: &mut Context,
    db: DbPool,
    selector: ScopeSelector,
    label: &'static str,
) -> JsObject {
    ObjectInitializer::new(context)
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, context| {
                        let (scope, offset) = runtime_scope(selector, args, context)?;
                        let collection = js_string_arg(
                            args.get(offset).and_then(|value| value.as_string()),
                            &format!("{label}.store.collection requires a collection name"),
                        )?;
                        validate_store_name(
                            &collection,
                            "$store collection name must be 1-64 chars using letters, numbers, _, -, or .",
                        )?;
                        let indexes = schema_indexes_arg(args.get(offset + 1), context)?;
                        let insert_db = db.clone();
                        let get_db = db.clone();
                        let list_db = db.clone();
                        let insert_collection = collection.clone();
                        let get_collection = collection.clone();
                        let list_collection = collection;
                        let insert_indexes = indexes.clone();
                        let list_indexes = indexes;
                        let collection_object = ObjectInitializer::new(context)
                            .function(
                                NativeFunction::from_closure_with_captures(
                                    move |_, args, _, context| {
                                        let value = object_arg(
                                            args.first(),
                                            "$store.collection(...).insert requires an object value",
                                            context,
                                        )?;
                                        let indexes =
                                            index_entries_from_value(&value, &insert_indexes)?;
                                        let runtime = with_runtime_context(|ctx| ctx.clone())
                                            .map_err(|e| js_err(e.to_string()))?;
                                        check_runtime_deadline()
                                            .map_err(|e| js_err(e.to_string()))?;
                                        let doc = if let Some(result) = with_judge_conn(|conn| {
                                            block_on_db(puzzle_backend::insert_store_doc_conn(
                                                conn,
                                                runtime.game_id,
                                                scope,
                                                &insert_collection,
                                                runtime.user_id,
                                                &value,
                                                &indexes,
                                            ))
                                        }) {
                                            result
                                        } else {
                                            block_on_db(puzzle_backend::insert_store_doc(
                                                &insert_db,
                                                runtime.game_id,
                                                scope,
                                                &insert_collection,
                                                runtime.user_id,
                                                &value,
                                                &indexes,
                                            ))
                                        }
                                        .map_err(|e| js_err(e.to_string()))?;
                                        let json = serde_json::to_value(doc)
                                            .map_err(|e| js_err(e.to_string()))?;
                                        json_to_js(&json, context)
                                            .map_err(|e| js_err(e.to_string()))
                                    },
                                    (),
                                ),
                                js_string!("insert"),
                                1,
                            )
                            .function(
                                NativeFunction::from_closure_with_captures(
                                    move |_, args, _, context| {
                                        let doc_id = args
                                            .first()
                                            .and_then(|value| value.as_number())
                                            .ok_or_else(|| {
                                                js_err(
                                                    "$store.collection(...).get requires a document id",
                                                )
                                            })?
                                            as i64;
                                        let runtime = with_runtime_context(|ctx| ctx.clone())
                                            .map_err(|e| js_err(e.to_string()))?;
                                        check_runtime_deadline()
                                            .map_err(|e| js_err(e.to_string()))?;
                                        let doc = if let Some(result) = with_judge_conn(|conn| {
                                            block_on_db(puzzle_backend::get_store_doc_conn(
                                                conn,
                                                runtime.game_id,
                                                scope,
                                                &get_collection,
                                                doc_id,
                                            ))
                                        }) {
                                            result
                                        } else {
                                            block_on_db(puzzle_backend::get_store_doc(
                                                &get_db,
                                                runtime.game_id,
                                                scope,
                                                &get_collection,
                                                doc_id,
                                            ))
                                        }
                                        .map_err(|e| js_err(e.to_string()))?;
                                        let json = serde_json::to_value(doc)
                                            .map_err(|e| js_err(e.to_string()))?;
                                        json_to_js(&json, context)
                                            .map_err(|e| js_err(e.to_string()))
                                    },
                                    (),
                                ),
                                js_string!("get"),
                                1,
                            )
                            .function(
                                NativeFunction::from_closure_with_captures(
                                    move |_, args, _, context| {
                                        let options = args
                                            .first()
                                            .map(|value| js_to_json(value, context))
                                            .transpose()
                                            .map_err(|e| js_err(e.to_string()))?
                                            .unwrap_or(Value::Object(Map::new()));
                                        if !options.is_object() {
                                            return Err(js_err(
                                                "$store.collection(...).list options must be an object",
                                            ));
                                        }
                                        let options =
                                            list_options_from_value(&options, &list_indexes)?;
                                        let runtime = with_runtime_context(|ctx| ctx.clone())
                                            .map_err(|e| js_err(e.to_string()))?;
                                        check_runtime_deadline()
                                            .map_err(|e| js_err(e.to_string()))?;
                                        let docs = if let Some(result) = with_judge_conn(|conn| {
                                            block_on_db(puzzle_backend::list_store_docs_conn(
                                                conn,
                                                runtime.game_id,
                                                scope,
                                                &list_collection,
                                                &options,
                                            ))
                                        }) {
                                            result
                                        } else {
                                            block_on_db(puzzle_backend::list_store_docs(
                                                &list_db,
                                                runtime.game_id,
                                                scope,
                                                &list_collection,
                                                &options,
                                            ))
                                        }
                                        .map_err(|e| js_err(e.to_string()))?;
                                        let json = serde_json::to_value(docs)
                                            .map_err(|e| js_err(e.to_string()))?;
                                        json_to_js(&json, context)
                                            .map_err(|e| js_err(e.to_string()))
                                    },
                                    (),
                                ),
                                js_string!("list"),
                                1,
                            )
                            .build();
                        Ok(collection_object.into())
                    },
                    (),
                )
            },
            js_string!("collection"),
            3,
        )
        .build()
}

fn backend_currency_json(
    currency: Option<crate::db::team::RbCurrencyShowData>,
) -> serde_json::Result<Value> {
    serde_json::to_value(currency.map(crate::db::team::PuzzleBackendCurrencyShowData::from))
}

fn backend_currencies_json(
    currencies: Vec<crate::db::team::RbCurrencyShowData>,
) -> serde_json::Result<Value> {
    serde_json::to_value(
        currencies
            .into_iter()
            .map(crate::db::team::PuzzleBackendCurrencyShowData::from)
            .collect::<Vec<_>>(),
    )
}

fn currency_update_options_arg(
    value: &JsValue,
    context: &mut Context,
) -> Result<crate::db::team::UpdateCurrencyOptions, JsError> {
    if let Some(amount) = value.as_number() {
        return Ok(crate::db::team::UpdateCurrencyOptions {
            amount: Some(js_number_to_i64(
                amount,
                "currency.update amount must be an integer in i64 range",
            )?),
            team_growth: None,
            hidden: None,
        });
    }

    let json = js_to_json(value, context).map_err(|e| js_err(e.to_string()))?;
    let object = json
        .as_object()
        .ok_or_else(|| js_err("currency.update options must be an object"))?;
    let number_field = |name: &str| -> Result<Option<i64>, JsError> {
        object
            .get(name)
            .map(|value| {
                value.as_i64().ok_or_else(|| {
                    js_err(format!("currency.update options.{name} must be a number"))
                })
            })
            .transpose()
    };
    Ok(crate::db::team::UpdateCurrencyOptions {
        amount: number_field("amount")?,
        team_growth: number_field("teamGrowth")?,
        hidden: object
            .get("hidden")
            .map(|value| {
                value
                    .as_bool()
                    .ok_or_else(|| js_err("currency.update options.hidden must be a boolean"))
            })
            .transpose()?,
    })
}

fn build_currency_object(
    context: &mut Context,
    db: DbPool,
    selector: CurrencySelector,
) -> JsObject {
    let query_db = db.clone();
    let cost_db = db.clone();
    let add_db = db.clone();
    let update_db = db;
    ObjectInitializer::new(context)
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, context| {
                        let (team_id, offset, check_team) = runtime_currency_team(selector, args)?;
                        let runtime = with_runtime_context(|ctx| ctx.clone())
                            .map_err(|e| js_err(e.to_string()))?;
                        check_runtime_deadline().map_err(|e| js_err(e.to_string()))?;
                        let currency_id = args
                            .get(offset)
                            .map(|value| {
                                currency_ref_arg(
                                    Some(value),
                                    "currency.query requires currency id or slug",
                                )
                            })
                            .transpose()?;
                        if let Some(result) = with_judge_conn(|conn| {
                            if check_team {
                                require_currency_team_in_game_conn(conn, team_id, runtime.game_id)
                                    .map_err(|e| internal_err(e.to_string()))?;
                            }
                            match &currency_id {
                                Some(CurrencyRef::Id(currency_id)) => {
                                    let row = block_on_db(
                                        crate::db::team::get_currency_info_one_all_conn(
                                            conn,
                                            team_id,
                                            *currency_id,
                                        ),
                                    )?;
                                    backend_currency_json(row)
                                        .map_err(|e| internal_err(e.to_string()))
                                }
                                Some(CurrencyRef::Slug(slug)) => {
                                    let row = block_on_db(
                                        crate::db::team::get_currency_info_one_by_slug_all_conn(
                                            conn,
                                            team_id,
                                            runtime.game_id,
                                            slug,
                                        ),
                                    )?;
                                    backend_currency_json(row)
                                        .map_err(|e| internal_err(e.to_string()))
                                }
                                None => {
                                    let rows = block_on_db(
                                        crate::db::team::get_currency_info_all_conn(conn, team_id),
                                    )?;
                                    backend_currencies_json(rows)
                                        .map_err(|e| internal_err(e.to_string()))
                                }
                            }
                        }) {
                            let json = result.map_err(|e| js_err(e.to_string()))?;
                            return json_to_js(&json, context).map_err(|e| js_err(e.to_string()));
                        }
                        if check_team {
                            require_currency_team_in_game(&query_db, team_id, runtime.game_id)?;
                        }
                        match currency_id {
                            Some(CurrencyRef::Id(currency_id)) => {
                                let row = block_on_db(crate::db::team::get_currency_info_one_all(
                                    &query_db,
                                    team_id,
                                    currency_id,
                                ))
                                .map_err(|e| js_err(e.to_string()))?;
                                let json = backend_currency_json(row)
                                    .map_err(|e| js_err(e.to_string()))?;
                                json_to_js(&json, context).map_err(|e| js_err(e.to_string()))
                            }
                            Some(CurrencyRef::Slug(slug)) => {
                                let row = block_on_db(
                                    crate::db::team::get_currency_info_one_by_slug_all(
                                        &query_db,
                                        team_id,
                                        runtime.game_id,
                                        &slug,
                                    ),
                                )
                                .map_err(|e| js_err(e.to_string()))?;
                                let json = backend_currency_json(row)
                                    .map_err(|e| js_err(e.to_string()))?;
                                json_to_js(&json, context).map_err(|e| js_err(e.to_string()))
                            }
                            None => {
                                let rows = block_on_db(crate::db::team::get_currency_info_all(
                                    &query_db, team_id,
                                ))
                                .map_err(|e| js_err(e.to_string()))?;
                                let json = backend_currencies_json(rows)
                                    .map_err(|e| js_err(e.to_string()))?;
                                json_to_js(&json, context).map_err(|e| js_err(e.to_string()))
                            }
                        }
                    },
                    (),
                )
            },
            js_string!("query"),
            2,
        )
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, _context| {
                        let (team_id, offset, check_team) = runtime_currency_team(selector, args)?;
                        let runtime = with_runtime_context(|ctx| ctx.clone())
                            .map_err(|e| js_err(e.to_string()))?;
                        check_runtime_deadline().map_err(|e| js_err(e.to_string()))?;
                        let currency_id = currency_ref_arg(
                            args.get(offset),
                            "currency.cost requires currency id or slug",
                        )?;
                        let amount = args
                            .get(offset + 1)
                            .and_then(|value| value.as_number())
                            .ok_or_else(|| js_err("currency.cost requires amount"))
                            .and_then(|value| {
                                js_number_to_i64(
                                    value,
                                    "currency.cost amount must be an integer in i64 range",
                                )
                            })?;
                        let reason = optional_reason_arg(
                            args.get(offset + 2),
                            "currency.cost reason must be a string or null",
                        )?;
                        let context = crate::db::team::CurrencyEventContext {
                            puzzle_id: Some(runtime.puzzle_id),
                            puzzle_title: Some(&runtime.puzzle_title),
                            reason: reason.as_deref(),
                        };
                        if let Some(result) = with_judge_conn(|conn| {
                            if check_team {
                                require_currency_team_in_game_conn(conn, team_id, runtime.game_id)
                                    .map_err(|e| internal_err(e.to_string()))?;
                            }
                            match &currency_id {
                                CurrencyRef::Id(currency_id) => {
                                    block_on_db(crate::db::team::cost_currency_conn(
                                        conn,
                                        team_id,
                                        *currency_id,
                                        amount,
                                        Some(context),
                                    ))
                                }
                                CurrencyRef::Slug(slug) => {
                                    block_on_db(crate::db::team::cost_currency_by_slug_conn(
                                        conn,
                                        team_id,
                                        runtime.game_id,
                                        slug,
                                        amount,
                                        Some(context),
                                    ))
                                }
                            }
                        }) {
                            return result.map(JsValue::from).map_err(|e| js_err(e.to_string()));
                        }
                        if check_team {
                            require_currency_team_in_game(&cost_db, team_id, runtime.game_id)?;
                        }
                        let updated = match currency_id {
                            CurrencyRef::Id(currency_id) => {
                                block_on_db(crate::db::team::cost_currency(
                                    &cost_db,
                                    team_id,
                                    currency_id,
                                    amount,
                                    Some(context),
                                ))
                            }
                            CurrencyRef::Slug(slug) => {
                                block_on_db(crate::db::team::cost_currency_by_slug(
                                    &cost_db,
                                    team_id,
                                    runtime.game_id,
                                    &slug,
                                    amount,
                                    Some(context),
                                ))
                            }
                        }
                        .map_err(|e| js_err(e.to_string()))?;
                        Ok(JsValue::from(updated))
                    },
                    (),
                )
            },
            js_string!("cost"),
            4,
        )
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, _context| {
                        let (team_id, offset, check_team) = runtime_currency_team(selector, args)?;
                        let runtime = with_runtime_context(|ctx| ctx.clone())
                            .map_err(|e| js_err(e.to_string()))?;
                        check_runtime_deadline().map_err(|e| js_err(e.to_string()))?;
                        let currency_id = currency_ref_arg(
                            args.get(offset),
                            "currency.add requires currency id or slug",
                        )?;
                        let amount = args
                            .get(offset + 1)
                            .and_then(|value| value.as_number())
                            .ok_or_else(|| js_err("currency.add requires amount"))
                            .and_then(|value| {
                                js_number_to_i64(
                                    value,
                                    "currency.add amount must be an integer in i64 range",
                                )
                            })?;
                        let reason = optional_reason_arg(
                            args.get(offset + 2),
                            "currency.add reason must be a string or null",
                        )?;
                        let context = crate::db::team::CurrencyEventContext {
                            puzzle_id: Some(runtime.puzzle_id),
                            puzzle_title: Some(&runtime.puzzle_title),
                            reason: reason.as_deref(),
                        };
                        if let Some(result) = with_judge_conn(|conn| {
                            if check_team {
                                require_currency_team_in_game_conn(conn, team_id, runtime.game_id)
                                    .map_err(|e| internal_err(e.to_string()))?;
                            }
                            match &currency_id {
                                CurrencyRef::Id(currency_id) => {
                                    block_on_db(crate::db::team::add_currency_conn(
                                        conn,
                                        team_id,
                                        *currency_id,
                                        amount,
                                        Some(context),
                                    ))
                                }
                                CurrencyRef::Slug(slug) => {
                                    block_on_db(crate::db::team::add_currency_by_slug_conn(
                                        conn,
                                        team_id,
                                        runtime.game_id,
                                        slug,
                                        amount,
                                        Some(context),
                                    ))
                                }
                            }
                        }) {
                            return result
                                .map(|updated| match updated {
                                    Some(delta) => JsValue::from(delta),
                                    None => JsValue::null(),
                                })
                                .map_err(|e| js_err(e.to_string()));
                        }
                        if check_team {
                            require_currency_team_in_game(&add_db, team_id, runtime.game_id)?;
                        }
                        let updated = match currency_id {
                            CurrencyRef::Id(currency_id) => {
                                block_on_db(crate::db::team::add_currency(
                                    &add_db,
                                    team_id,
                                    currency_id,
                                    amount,
                                    Some(context),
                                ))
                            }
                            CurrencyRef::Slug(slug) => {
                                block_on_db(crate::db::team::add_currency_by_slug(
                                    &add_db,
                                    team_id,
                                    runtime.game_id,
                                    &slug,
                                    amount,
                                    Some(context),
                                ))
                            }
                        }
                        .map_err(|e| js_err(e.to_string()))?;
                        Ok(match updated {
                            Some(delta) => JsValue::from(delta),
                            None => JsValue::null(),
                        })
                    },
                    (),
                )
            },
            js_string!("add"),
            4,
        )
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, context| {
                        let (team_id, offset, check_team) = runtime_currency_team(selector, args)?;
                        let runtime = with_runtime_context(|ctx| ctx.clone())
                            .map_err(|e| js_err(e.to_string()))?;
                        check_runtime_deadline().map_err(|e| js_err(e.to_string()))?;
                        let currency_id = currency_ref_arg(
                            args.get(offset),
                            "currency.update requires currency id or slug",
                        )?;
                        let value = args
                            .get(offset + 1)
                            .ok_or_else(|| js_err("currency.update requires amount or options"))?;
                        let options = currency_update_options_arg(value, context)?;
                        let reason = optional_reason_arg(
                            args.get(offset + 2),
                            "currency.update reason must be a string or null",
                        )?;
                        let event_context = crate::db::team::CurrencyEventContext {
                            puzzle_id: Some(runtime.puzzle_id),
                            puzzle_title: Some(&runtime.puzzle_title),
                            reason: reason.as_deref(),
                        };
                        if let Some(result) = with_judge_conn(|conn| {
                            if check_team {
                                require_currency_team_in_game_conn(conn, team_id, runtime.game_id)
                                    .map_err(|e| internal_err(e.to_string()))?;
                            }
                            match &currency_id {
                                CurrencyRef::Id(currency_id) => {
                                    block_on_db(crate::db::team::update_currency_conn(
                                        conn,
                                        team_id,
                                        *currency_id,
                                        options,
                                        Some(event_context),
                                    ))
                                }
                                CurrencyRef::Slug(slug) => {
                                    block_on_db(crate::db::team::update_currency_by_slug_conn(
                                        conn,
                                        team_id,
                                        runtime.game_id,
                                        slug,
                                        options,
                                        Some(event_context),
                                    ))
                                }
                            }
                        }) {
                            let json =
                                backend_currency_json(result.map_err(|e| js_err(e.to_string()))?)
                                    .map_err(|e| js_err(e.to_string()))?;
                            return json_to_js(&json, context).map_err(|e| js_err(e.to_string()));
                        }
                        if check_team {
                            require_currency_team_in_game(&update_db, team_id, runtime.game_id)?;
                        }
                        let updated = match currency_id {
                            CurrencyRef::Id(currency_id) => {
                                block_on_db(crate::db::team::update_currency(
                                    &update_db,
                                    team_id,
                                    currency_id,
                                    options,
                                    Some(event_context),
                                ))
                            }
                            CurrencyRef::Slug(slug) => {
                                block_on_db(crate::db::team::update_currency_by_slug(
                                    &update_db,
                                    team_id,
                                    runtime.game_id,
                                    &slug,
                                    options,
                                    Some(event_context),
                                ))
                            }
                        }
                        .map_err(|e| js_err(e.to_string()))?;
                        let json =
                            backend_currency_json(updated).map_err(|e| js_err(e.to_string()))?;
                        json_to_js(&json, context).map_err(|e| js_err(e.to_string()))
                    },
                    (),
                )
            },
            js_string!("update"),
            4,
        )
        .build()
}

fn build_assets_object(context: &mut Context, asset_runtime: AssetRuntime) -> JsObject {
    let asset_list_runtime = asset_runtime.clone();
    let asset_read_text_runtime = asset_runtime.clone();
    let asset_read_json_runtime = asset_runtime.clone();
    let asset_read_bytes_runtime = asset_runtime;
    ObjectInitializer::new(context)
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, context| {
                        let object_key = js_string_arg(
                            args.first().and_then(|value| value.as_string()),
                            "$puzzle.assets.list requires an object key",
                        )?;
                        let runtime = with_runtime_context(|ctx| ctx.clone())
                            .map_err(|e| js_err(e.to_string()))?;
                        let files = block_on_db(asset::list_readable_files_by_object_key(
                            &asset_list_runtime.db,
                            runtime.game_id,
                            runtime.puzzle_id,
                            &object_key,
                        ))
                        .map_err(|e| js_err(e.to_string()))?;
                        let json =
                            serde_json::to_value(files).map_err(|e| js_err(e.to_string()))?;
                        json_to_js(&json, context).map_err(|e| js_err(e.to_string()))
                    },
                    (),
                )
            },
            js_string!("list"),
            1,
        )
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, _context| {
                        let object_key = js_string_arg(
                            args.first().and_then(|value| value.as_string()),
                            "$puzzle.assets.readText requires an object key",
                        )?;
                        let relative_path = js_asset_path_arg(
                            args.get(1).and_then(|value| value.as_string()),
                            "$puzzle.assets.readText requires a relative path",
                        )?;
                        let bytes = read_asset_bytes(
                            &asset_read_text_runtime,
                            &object_key,
                            &relative_path,
                        )?;
                        let text = String::from_utf8(bytes).map_err(|_| {
                            js_err("$puzzle.assets.readText requires UTF-8 content")
                        })?;
                        Ok(JsValue::from(JsString::from(text)))
                    },
                    (),
                )
            },
            js_string!("readText"),
            2,
        )
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, context| {
                        let object_key = js_string_arg(
                            args.first().and_then(|value| value.as_string()),
                            "$puzzle.assets.readJson requires an object key",
                        )?;
                        let relative_path = js_asset_path_arg(
                            args.get(1).and_then(|value| value.as_string()),
                            "$puzzle.assets.readJson requires a relative path",
                        )?;
                        let bytes = read_asset_bytes(
                            &asset_read_json_runtime,
                            &object_key,
                            &relative_path,
                        )?;
                        let json: Value = serde_json::from_slice(&bytes)
                            .map_err(|e| js_err(format!("$puzzle.assets.readJson failed: {e}")))?;
                        json_to_js(&json, context).map_err(|e| js_err(e.to_string()))
                    },
                    (),
                )
            },
            js_string!("readJson"),
            2,
        )
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, context| {
                        let object_key = js_string_arg(
                            args.first().and_then(|value| value.as_string()),
                            "$puzzle.assets.readBytes requires an object key",
                        )?;
                        let relative_path = js_asset_path_arg(
                            args.get(1).and_then(|value| value.as_string()),
                            "$puzzle.assets.readBytes requires a relative path",
                        )?;
                        let bytes = read_asset_bytes(
                            &asset_read_bytes_runtime,
                            &object_key,
                            &relative_path,
                        )?;
                        let json =
                            serde_json::to_value(bytes).map_err(|e| js_err(e.to_string()))?;
                        json_to_js(&json, context).map_err(|e| js_err(e.to_string()))
                    },
                    (),
                )
            },
            js_string!("readBytes"),
            2,
        )
        .build()
}

fn build_submission_object(context: &mut Context, app: AppState) -> JsObject {
    ObjectInitializer::new(context)
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, context| {
                        if in_transactional_judge() {
                            return Err(js_err(
                                "$this.submission.add is not available in judge functions",
                            ));
                        }
                        let input_value = args
                            .first()
                            .ok_or_else(|| js_err("$this.submission.add requires an object"))?;
                        let input_json =
                            js_to_json(input_value, context).map_err(|e| js_err(e.to_string()))?;
                        let input = backend_submission_input_from_value(&input_json)
                            .map_err(|e| js_err(e.to_string()))?;
                        let runtime = with_runtime_context(|ctx| ctx.clone())
                            .map_err(|e| js_err(e.to_string()))?;
                        let row =
                            block_on_db(crate::db::puzzle::add_backend_submission_and_invalidate(
                                &app,
                                runtime.team_id,
                                runtime.user_id,
                                runtime.puzzle_id,
                                &input,
                            ))
                            .map_err(|e| js_err(e.to_string()))?;
                        let json = serde_json::to_value(row).map_err(|e| js_err(e.to_string()))?;
                        json_to_js(&json, context).map_err(|e| js_err(e.to_string()))
                    },
                    (),
                )
            },
            js_string!("add"),
            1,
        )
        .build()
}

fn valid_backend_event_name(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_alphabetic())
        && value.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn capture_backend_event(
    capture: &Arc<Mutex<Vec<EmittedPuzzleBackendEvent>>>,
    event: String,
    payload: Value,
) -> Result<(), String> {
    if !valid_backend_event_name(&event) {
        return Err(
            "$this.event.emit event name must start with a letter and contain at most 64 letters, numbers, _, -, or ."
                .to_string(),
        );
    }
    let payload_bytes = serde_json::to_vec(&payload)
        .map_err(|e| e.to_string())?
        .len();
    if payload_bytes > MAX_BACKEND_EVENT_PAYLOAD_BYTES {
        return Err(format!(
            "$this.event.emit payload exceeds {MAX_BACKEND_EVENT_PAYLOAD_BYTES} bytes",
        ));
    }

    let mut events = capture
        .lock()
        .map_err(|_| "$this.event.emit capture is unavailable".to_string())?;
    if events.len() >= MAX_BACKEND_EVENTS {
        return Err(format!(
            "$this.event.emit allows at most {MAX_BACKEND_EVENTS} events per execution",
        ));
    }
    events.push(EmittedPuzzleBackendEvent { event, payload });
    Ok(())
}

fn build_event_object(
    context: &mut Context,
    capture: Arc<Mutex<Vec<EmittedPuzzleBackendEvent>>>,
) -> JsObject {
    ObjectInitializer::new(context)
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, context| {
                        let event = js_string_arg(
                            args.first().and_then(|value| value.as_string()),
                            "$this.event.emit requires an event name",
                        )?;
                        let payload = args.get(1).cloned().unwrap_or_else(JsValue::null);
                        let payload =
                            js_to_json(&payload, context).map_err(|e| js_err(e.to_string()))?;
                        capture_backend_event(&capture, event, payload).map_err(js_err)?;
                        Ok(JsValue::undefined())
                    },
                    (),
                )
            },
            js_string!("emit"),
            2,
        )
        .build()
}

fn configure_runtime_limits(context: &mut Context) {
    let limits = context.runtime_limits_mut();
    limits.set_loop_iteration_limit(100_000);
    limits.set_recursion_limit(128);
    limits.set_stack_size_limit(1024 * 4);
    limits.set_backtrace_limit(16);
}

fn configure_judge_runtime_limits(context: &mut Context) {
    configure_runtime_limits(context);
    context
        .runtime_limits_mut()
        .set_loop_iteration_limit(JUDGE_LOOP_ITERATION_LIMIT);
}

pub fn register_ctx(
    context: &mut Context,
    services: RuntimeServices,
) -> Result<(), RbInternalError> {
    let app = services.app;
    let asset_runtime = services.asset_runtime;
    let db = app.db.clone();

    let game_kv = build_kv_object(context, db.clone(), ScopeSelector::GameArg, "$game");
    let team_kv = build_kv_object(context, db.clone(), ScopeSelector::Team, "$team");
    let puzzle_kv = build_kv_object(context, db.clone(), ScopeSelector::Puzzle, "$puzzle");
    let this_kv = build_kv_object(context, db.clone(), ScopeSelector::This, "$this");

    let game_store = build_store_object(context, db.clone(), ScopeSelector::GameArg, "$game");
    let team_store = build_store_object(context, db.clone(), ScopeSelector::Team, "$team");
    let puzzle_store = build_store_object(context, db.clone(), ScopeSelector::Puzzle, "$puzzle");
    let this_store = build_store_object(context, db.clone(), ScopeSelector::This, "$this");

    let game_currency = build_currency_object(context, db.clone(), CurrencySelector::GameArg);
    let team_currency = build_currency_object(context, db, CurrencySelector::Team);

    let puzzle_assets = build_assets_object(context, asset_runtime);
    let this_submission = build_submission_object(context, app.clone());
    let this_event = build_event_object(context, services.event_capture);
    let this_solve_app = app.clone();

    let runtime = with_runtime_context(|ctx| ctx.clone())?;

    let game = ObjectInitializer::new(context)
        .property(js_string!("id"), runtime.game_id, Attribute::all())
        .property(js_string!("kv"), game_kv, Attribute::all())
        .property(js_string!("store"), game_store, Attribute::all())
        .property(js_string!("currency"), game_currency, Attribute::all())
        .build();

    let team = ObjectInitializer::new(context)
        .property(js_string!("id"), runtime.team_id, Attribute::all())
        .property(
            js_string!("name"),
            JsValue::from(JsString::from(runtime.team_name.clone())),
            Attribute::all(),
        )
        .property(js_string!("kv"), team_kv, Attribute::all())
        .property(js_string!("store"), team_store, Attribute::all())
        .property(js_string!("currency"), team_currency, Attribute::all())
        .build();

    let puzzle = ObjectInitializer::new(context)
        .property(js_string!("id"), runtime.puzzle_id, Attribute::all())
        .property(js_string!("gameId"), runtime.game_id, Attribute::all())
        .property(
            js_string!("title"),
            JsValue::from(JsString::from(runtime.puzzle_title.clone())),
            Attribute::all(),
        )
        .property(js_string!("kv"), puzzle_kv, Attribute::all())
        .property(js_string!("store"), puzzle_store, Attribute::all())
        .property(js_string!("assets"), puzzle_assets, Attribute::all())
        .build();

    let this = ObjectInitializer::new(context)
        .property(js_string!("game"), game.clone(), Attribute::all())
        .property(js_string!("team"), team.clone(), Attribute::all())
        .property(js_string!("puzzle"), puzzle.clone(), Attribute::all())
        .property(js_string!("kv"), this_kv, Attribute::all())
        .property(js_string!("store"), this_store, Attribute::all())
        .property(js_string!("submission"), this_submission, Attribute::all())
        .property(js_string!("event"), this_event, Attribute::all())
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, context| {
                        if in_transactional_judge() {
                            return Err(js_err("$this.solve is not available in judge functions"));
                        }
                        let submission_value = args
                            .first()
                            .ok_or_else(|| js_err("$this.solve requires a submission"))?;
                        let submission_json = js_to_json(submission_value, context)
                            .map_err(|e| js_err(e.to_string()))?;
                        let submission = backend_submission_from_value(&submission_json)
                            .map_err(|e| js_err(e.to_string()))?;
                        let runtime = with_runtime_context(|ctx| ctx.clone())
                            .map_err(|e| js_err(e.to_string()))?;
                        let solved =
                            block_on_db(crate::db::puzzle::solve_backend_puzzle_with_submission(
                                &this_solve_app,
                                runtime.team_id,
                                runtime.user_id,
                                runtime.puzzle_id,
                                &submission,
                            ))
                            .map_err(|e| js_err(e.to_string()))?;
                        Ok(JsValue::from(solved))
                    },
                    (),
                )
            },
            js_string!("solve"),
            1,
        )
        .build();

    context
        .register_global_property(js_string!("$game"), game, Attribute::all())
        .map_err(|e| internal_err(e.to_string()))?;
    context
        .register_global_property(js_string!("$team"), team, Attribute::all())
        .map_err(|e| internal_err(e.to_string()))?;
    context
        .register_global_property(js_string!("$puzzle"), puzzle, Attribute::all())
        .map_err(|e| internal_err(e.to_string()))?;
    context
        .register_global_property(js_string!("$this"), this, Attribute::all())
        .map_err(|e| internal_err(e.to_string()))?;

    Ok(())
}

fn resolve_promise_value(
    value: JsValue,
    context: &mut Context,
) -> Result<JsValue, RbInternalError> {
    let Some(promise) = value.as_promise() else {
        return Ok(value);
    };

    context
        .run_jobs()
        .map_err(|e| internal_err(e.to_string()))?;
    match promise.state() {
        PromiseState::Fulfilled(value) => Ok(value.clone()),
        PromiseState::Rejected(reason) => {
            Err(internal_err(format!("api rejected: {}", reason.display())))
        }
        PromiseState::Pending => Err(internal_err("api promise is still pending")),
    }
}

fn execute_backend_function<T>(
    app: AppState,
    backend: crate::db::puzzle_backend::PuzzleBackend,
    function_name: String,
    runtime: RuntimeContext,
    build_args: impl FnOnce(&mut Context) -> Result<Vec<JsValue>, RbInternalError>,
    convert_result: impl FnOnce(JsValue, &mut Context) -> Result<T, RbInternalError>,
) -> Result<BackendExecution<T>, RbInternalError> {
    let log_runtime = runtime.clone();
    let event_capture = Arc::new(Mutex::new(Vec::new()));
    RUNTIME_CONTEXT.with(|slot| {
        *slot.borrow_mut() = Some(runtime);
    });
    let _guard = RuntimeContextGuard;

    let mut context = Context::default();
    let capture = Rc::new(RefCell::new(BackendConsoleCapture::default()));
    let result = (|| {
        let is_judge = with_runtime_context(|ctx| ctx.method == "JUDGE")?;
        if is_judge {
            configure_judge_runtime_limits(&mut context);
        } else {
            configure_runtime_limits(&mut context);
        }
        register_console(&mut context, capture.clone())?;
        register_ctx(
            &mut context,
            RuntimeServices {
                app: app.clone(),
                asset_runtime: AssetRuntime {
                    db: app.db.clone(),
                    storage: app.storage.clone(),
                    max_read_bytes: DEFAULT_MAX_ASSET_READ_BYTES,
                },
                event_capture: event_capture.clone(),
            },
        )?;

        let module = Module::parse(Source::from_bytes(&backend.source), None, &mut context)
            .map_err(|e| internal_err(e.to_string()))?;
        let promise = module.load_link_evaluate(&mut context);
        context
            .run_jobs()
            .map_err(|e| internal_err(e.to_string()))?;
        if let Some(err) = promise.state().as_rejected() {
            return Err(internal_err(format!("module rejected: {}", err.display())));
        }

        let namespace = module.namespace(&mut context);
        let value = namespace
            .get(js_string!(function_name.as_str()), &mut context)
            .map_err(|e| internal_err(e.to_string()))?;
        let function = value
            .as_function()
            .ok_or_else(|| RbInternalError::Other("export is not a function".to_string()))?;

        let args = build_args(&mut context)?;
        let result = function
            .call(&JsValue::undefined(), &args, &mut context)
            .map_err(|e| internal_err(e.to_string()))?;
        let result = resolve_promise_value(result, &mut context)?;

        convert_result(result, &mut context)
    })();

    let duration_ms =
        i64::try_from(log_runtime.started_at.elapsed().as_millis()).unwrap_or(i64::MAX);
    let (execution_type, request_method) = match log_runtime.method.as_str() {
        "JUDGE" => ("judge", None),
        "HINT_PURCHASE" => ("hint_purchase", None),
        method => ("api", Some(method)),
    };
    let error = result.as_ref().err().map(ToString::to_string);
    let capture = capture.borrow();
    let console = serde_json::to_value(&capture.entries).unwrap_or_else(|_| Value::Array(vec![]));
    if let Err(log_error) = block_on_db(crate::db::puzzle_backend::log_call(
        &app.db,
        crate::db::puzzle_backend::PuzzleBackendCallLogInput {
            puzzle_id: log_runtime.puzzle_id,
            team_id: Some(log_runtime.team_id),
            user_id: log_runtime.user_id,
            execution_type,
            request_method,
            function_name: &function_name,
            ok: result.is_ok(),
            duration_ms,
            submission_id: log_runtime.submission_id,
            hint_id: log_runtime.hint_id,
            error: error.as_deref(),
            console: &console,
            console_truncated: capture.truncated,
        },
    )) {
        log::warn!(
            "failed to write puzzle backend execution log for puzzle {} function {}: {log_error}",
            log_runtime.puzzle_id,
            function_name
        );
    }

    let events = if result.is_ok() {
        event_capture
            .lock()
            .map_err(|_| internal_err("puzzle backend event capture is unavailable"))?
            .drain(..)
            .map(|event| PuzzleBackendEventSync {
                puzzle_id: log_runtime.puzzle_id,
                user_id: log_runtime.user_id,
                user_nickname: log_runtime.user_nickname.clone(),
                event: event.event,
                payload: event.payload,
                source_type: match log_runtime.method.as_str() {
                    "JUDGE" => "judge",
                    "HINT_PURCHASE" => "hintPurchase",
                    _ => "api",
                },
                function: function_name.clone(),
            })
            .collect()
    } else {
        vec![]
    };

    result.map(|value| BackendExecution { value, events })
}

pub async fn execute_api(
    app: &AppState,
    backend: crate::db::puzzle_backend::PuzzleBackend,
    api_name: String,
    runtime: RuntimeContext,
) -> Result<BackendExecution<Value>, RbInternalError> {
    let app = app.clone();
    let handle = Handle::current();
    tokio::task::spawn_blocking(move || {
        let _handle_guard = set_tokio_handle(handle);
        execute_backend_function(
            app,
            backend,
            api_name,
            runtime,
            |context| Ok(vec![build_ctx_arg(context)?]),
            |result, context| js_to_json(&result, context),
        )
    })
    .await
    .map_err(|e| internal_err(e.to_string()))?
}

pub async fn execute_judge(
    app: &AppState,
    backend: crate::db::puzzle_backend::PuzzleBackend,
    function_name: String,
    runtime: JudgeRuntimeContext,
) -> Result<BackendExecution<Option<crate::game::judge::JudgeBackendOutput>>, RbInternalError> {
    let app = app.clone();
    let handle = Handle::current();
    tokio::task::spawn_blocking(move || {
        let _handle_guard = set_tokio_handle(handle);
        let runtime_context = RuntimeContext {
            game_id: runtime.game_id,
            method: "JUDGE".to_string(),
            puzzle_id: runtime.puzzle_id,
            team_id: runtime.team_id,
            user_id: runtime.user_id,
            api_name: function_name.clone(),
            submission_id: Some(runtime.submission.id),
            hint_id: None,
            query: Value::Null,
            body: Value::Null,
            puzzle_title: runtime.puzzle_title.clone(),
            user_nickname: runtime.user_nickname.clone(),
            team_name: runtime.team_name.clone(),
            started_at: Instant::now(),
            timeout: JUDGE_EXECUTION_TIMEOUT,
        };
        execute_backend_function(
            app,
            backend,
            function_name,
            runtime_context,
            |context| Ok(vec![build_judge_ctx_arg(context, &runtime)?]),
            |result, context| {
                let Some(json) = js_to_json_optional(&result, context)? else {
                    return Ok(None);
                };
                let output: crate::game::judge::JudgeBackendOutput =
                    serde_json::from_value(json).map_err(|e| internal_err(e.to_string()))?;

                if output.ignored.is_some() && output.action.is_none() {
                    return Err(internal_err("judge output ignored requires action"));
                }

                Ok(Some(output))
            },
        )
    })
    .await
    .map_err(|e| internal_err(e.to_string()))?
}

pub async fn execute_judge_conn(
    app: &AppState,
    conn: &mut PgConnection,
    backend: crate::db::puzzle_backend::PuzzleBackend,
    function_name: String,
    runtime: JudgeRuntimeContext,
) -> Result<BackendExecution<Option<crate::game::judge::JudgeBackendOutput>>, RbInternalError> {
    sqlx::query!("SET LOCAL statement_timeout = '500ms';")
        .execute(&mut *conn)
        .await?;
    sqlx::query!("SET LOCAL lock_timeout = '500ms';")
        .execute(&mut *conn)
        .await?;

    let runtime_context = RuntimeContext {
        game_id: runtime.game_id,
        method: "JUDGE".to_string(),
        puzzle_id: runtime.puzzle_id,
        team_id: runtime.team_id,
        user_id: runtime.user_id,
        api_name: function_name.clone(),
        submission_id: Some(runtime.submission.id),
        hint_id: None,
        query: Value::Null,
        body: Value::Null,
        puzzle_title: runtime.puzzle_title.clone(),
        user_nickname: runtime.user_nickname.clone(),
        team_name: runtime.team_name.clone(),
        started_at: Instant::now(),
        timeout: JUDGE_EXECUTION_TIMEOUT,
    };

    let handle = Handle::current();
    let result = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                JUDGE_CONN.with(|slot| {
                    *slot.borrow_mut() = Some(conn as *mut PgConnection);
                });
                let _conn_guard = JudgeConnGuard;
                let _handle_guard = set_tokio_handle(handle);
                execute_backend_function(
                    app.clone(),
                    backend,
                    function_name,
                    runtime_context,
                    |context| Ok(vec![build_judge_ctx_arg(context, &runtime)?]),
                    |result, context| {
                        let Some(json) = js_to_json_optional(&result, context)? else {
                            return Ok(None);
                        };
                        let output: crate::game::judge::JudgeBackendOutput =
                            serde_json::from_value(json)
                                .map_err(|e| internal_err(e.to_string()))?;

                        if output.ignored.is_some() && output.action.is_none() {
                            return Err(internal_err("judge output ignored requires action"));
                        }

                        Ok(Some(output))
                    },
                )
            })
            .join()
            .map_err(|_| internal_err("judge function panicked"))
    })?;

    if result.is_ok() {
        sqlx::query!("SET LOCAL statement_timeout = DEFAULT;")
            .execute(&mut *conn)
            .await?;
        sqlx::query!("SET LOCAL lock_timeout = DEFAULT;")
            .execute(&mut *conn)
            .await?;
    }

    result
}

pub async fn execute_hint_purchase(
    app: &AppState,
    backend: crate::db::puzzle_backend::PuzzleBackend,
    function_name: String,
    runtime: HintPurchaseRuntimeContext,
) -> Result<BackendExecution<()>, RbInternalError> {
    let app = app.clone();
    let handle = Handle::current();
    tokio::task::spawn_blocking(move || {
        let _handle_guard = set_tokio_handle(handle);
        let runtime_context = RuntimeContext {
            game_id: runtime.game_id,
            method: "HINT_PURCHASE".to_string(),
            puzzle_id: runtime.puzzle_id,
            team_id: runtime.team_id,
            user_id: runtime.user_id,
            api_name: function_name.clone(),
            submission_id: None,
            hint_id: Some(runtime.hint_id),
            query: Value::Null,
            body: Value::Null,
            puzzle_title: runtime.puzzle_title.clone(),
            user_nickname: runtime.user_nickname.clone(),
            team_name: runtime.team_name.clone(),
            started_at: Instant::now(),
            timeout: DEFAULT_BACKEND_FUNCTION_TIMEOUT,
        };
        let ctx_function_name = function_name.clone();
        execute_backend_function(
            app,
            backend,
            function_name,
            runtime_context,
            |context| {
                Ok(vec![build_hint_purchase_ctx_arg(
                    context,
                    &runtime,
                    &ctx_function_name,
                )?])
            },
            |_, _| Ok(()),
        )
    })
    .await
    .map_err(|e| internal_err(e.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_capture_truncates_at_utf8_boundary() {
        let mut capture = BackendConsoleCapture::default();
        capture.push("log", "测".repeat(MAX_CONSOLE_ENTRY_BYTES));

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
    fn console_capture_limits_entry_count() {
        let mut capture = BackendConsoleCapture::default();
        for index in 0..=MAX_CONSOLE_ENTRIES {
            capture.push("info", index.to_string());
        }

        assert_eq!(capture.entries.len(), MAX_CONSOLE_ENTRIES);
        assert!(capture.truncated);
    }

    #[test]
    fn console_capture_limits_total_bytes() {
        let mut capture = BackendConsoleCapture::default();
        for _ in 0..=MAX_CONSOLE_TOTAL_BYTES / MAX_CONSOLE_ENTRY_BYTES {
            capture.push("warn", "x".repeat(MAX_CONSOLE_ENTRY_BYTES));
        }

        assert_eq!(capture.bytes, MAX_CONSOLE_TOTAL_BYTES);
        assert!(capture.truncated);
    }

    #[test]
    fn registered_console_captures_levels_and_arguments() {
        let mut context = Context::default();
        let capture = Rc::new(RefCell::new(BackendConsoleCapture::default()));
        register_console(&mut context, capture.clone()).expect("console should register");

        context
            .eval(Source::from_bytes(
                "console.log('value', 42); console.error('failed');",
            ))
            .expect("console calls should succeed");

        let capture = capture.borrow();
        assert_eq!(capture.entries.len(), 2);
        assert_eq!(capture.entries[0].level, "log");
        assert_eq!(capture.entries[0].message, "value 42");
        assert_eq!(capture.entries[1].level, "error");
        assert_eq!(capture.entries[1].message, "failed");
    }

    #[test]
    fn judge_context_exposes_only_stable_submission_metadata() {
        let created_at = time::OffsetDateTime::from_unix_timestamp(1_700_000_000)
            .expect("timestamp should be valid");
        let runtime = JudgeRuntimeContext {
            puzzle_id: 3,
            game_id: 4,
            puzzle_title: "Puzzle".to_string(),
            team_id: 5,
            team_name: "Team".to_string(),
            user_id: 6,
            user_nickname: "User".to_string(),
            user_answer: " Answer ".to_string(),
            norm_answer: "answer".to_string(),
            submission: crate::db::puzzle::BackendSubmissionShowData {
                id: 7,
                team_id: 5,
                user_id: 6,
                puzzle_id: 3,
                user_answer: " Answer ".to_string(),
                norm_answer: "answer".to_string(),
                saction: crate::model::game::RbJudgeAction::Pending,
                sresult: None,
                real_answer: None,
                ignored: false,
                ctime_at: created_at,
            },
        };
        let mut context = Context::default();

        let value =
            build_judge_ctx_arg(&mut context, &runtime).expect("judge context should build");
        let value = js_to_json(&value, &mut context).expect("judge context should be JSON");

        assert_eq!(value["request"]["userAnswer"], " Answer ");
        assert_eq!(value["request"]["normAnswer"], "answer");
        assert_eq!(
            value["submission"],
            json!({
                "id": 7,
                "createdAt": crate::serde_helpers::format_offset_datetime(&created_at),
            })
        );
    }

    #[test]
    fn backend_event_names_are_validated() {
        for name in ["level_completed", "level.completed", "Level-2"] {
            assert!(valid_backend_event_name(name));
        }
        for name in ["", "2level", ".level", "level completed"] {
            assert!(!valid_backend_event_name(name));
        }
        assert!(!valid_backend_event_name(&"a".repeat(65)));
    }

    #[test]
    fn backend_event_capture_preserves_order_and_limits_count() {
        let capture = Arc::new(Mutex::new(Vec::new()));
        for index in 0..MAX_BACKEND_EVENTS {
            capture_backend_event(&capture, format!("event_{index}"), Value::from(index))
                .expect("event should be captured");
        }

        let events = capture.lock().expect("capture should be available");
        assert_eq!(events.len(), MAX_BACKEND_EVENTS);
        assert_eq!(events[0].event, "event_0");
        assert_eq!(
            events[MAX_BACKEND_EVENTS - 1].payload,
            Value::from(MAX_BACKEND_EVENTS - 1)
        );
        drop(events);

        assert!(capture_backend_event(&capture, "overflow".to_string(), Value::Null).is_err());
    }

    #[test]
    fn backend_event_capture_limits_payload_size() {
        let capture = Arc::new(Mutex::new(Vec::new()));
        let oversized = Value::String("x".repeat(MAX_BACKEND_EVENT_PAYLOAD_BYTES));

        assert!(capture_backend_event(&capture, "large".to_string(), oversized).is_err());
        assert!(
            capture
                .lock()
                .expect("capture should be available")
                .is_empty()
        );
    }

    #[test]
    fn backend_event_object_emits_null_payload_by_default() {
        let mut context = Context::default();
        let capture = Arc::new(Mutex::new(Vec::new()));
        let event = build_event_object(&mut context, capture.clone());
        context
            .register_global_property(js_string!("event"), event, Attribute::all())
            .expect("event object should register");

        context
            .eval(Source::from_bytes("event.emit('ready')"))
            .expect("event should emit");

        let events = capture.lock().expect("capture should be available");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "ready");
        assert_eq!(events[0].payload, Value::Null);
    }

    #[test]
    fn kv_expiry_options_distinguish_omitted_null_and_ttl() {
        let mut context = Context::default();
        assert_eq!(
            kv_expiry_arg(
                None,
                &mut context,
                puzzle_backend::PuzzleBackendKvExpiry::Preserve,
                "$this.kv.compareAndSet",
            )
            .expect("omitted options should use the default"),
            puzzle_backend::PuzzleBackendKvExpiry::Preserve
        );

        let permanent = JsValue::from_json(&json!({ "ttl": null }), &mut context)
            .expect("options should convert to JS");
        assert_eq!(
            kv_expiry_arg(
                Some(&permanent),
                &mut context,
                puzzle_backend::PuzzleBackendKvExpiry::Preserve,
                "$this.kv.compareAndSet",
            )
            .expect("null TTL should be accepted"),
            puzzle_backend::PuzzleBackendKvExpiry::Permanent
        );

        let ttl = context
            .eval(Source::from_bytes("({ ttl: 30000 })"))
            .expect("JavaScript options should evaluate");
        assert_eq!(
            kv_expiry_arg(
                Some(&ttl),
                &mut context,
                puzzle_backend::PuzzleBackendKvExpiry::Permanent,
                "$this.kv.setIfAbsent",
            )
            .expect("valid TTL should be accepted"),
            puzzle_backend::PuzzleBackendKvExpiry::Ttl(30_000)
        );
    }

    #[test]
    fn kv_expiry_options_reject_out_of_range_ttl() {
        let mut context = Context::default();
        for ttl_ms in [0, MAX_KV_TTL_MS + 1] {
            let options = JsValue::from_json(&json!({ "ttl": ttl_ms }), &mut context)
                .expect("options should convert to JS");
            assert!(
                kv_expiry_arg(
                    Some(&options),
                    &mut context,
                    puzzle_backend::PuzzleBackendKvExpiry::Permanent,
                    "$this.kv.setIfAbsent",
                )
                .is_err()
            );
        }
    }

    #[test]
    fn kv_increment_defaults_and_validates_amount() {
        assert_eq!(
            kv_increment_amount_arg(None, "$this.kv.increment")
                .expect("omitted amount should default"),
            1.0
        );
        assert_eq!(
            kv_increment_amount_arg(Some(&JsValue::undefined()), "$this.kv.increment")
                .expect("undefined amount should default"),
            1.0
        );
        assert!(kv_increment_amount_arg(Some(&JsValue::nan()), "$this.kv.increment").is_err());
        assert!(kv_increment_amount_arg(Some(&JsValue::null()), "$this.kv.increment").is_err());
    }

    #[test]
    fn kv_entry_versions_are_exposed_as_strings() {
        let entry = kv_entry_json(puzzle_backend::PuzzleBackendKvValue {
            value: json!({ "ready": true }),
            version: 9_007_199_254_740_993,
            expires_at: None,
        });

        assert_eq!(entry["version"], "9007199254740993");
        assert_eq!(entry["value"], json!({ "ready": true }));
        assert_eq!(entry["expiresAt"], Value::Null);
    }

    #[test]
    fn currency_update_options_use_camel_case_team_growth() {
        let mut context = Context::default();
        let value = JsValue::from_json(
            &json!({ "amount": 10, "teamGrowth": 2, "hidden": true }),
            &mut context,
        )
        .expect("options should convert to JS");

        let options = currency_update_options_arg(&value, &mut context)
            .expect("team growth options should be accepted");
        assert_eq!(options.amount, Some(10));
        assert_eq!(options.team_growth, Some(2));
        assert_eq!(options.hidden, Some(true));
    }

    #[test]
    fn backend_submission_uses_action_and_result_names() {
        let input = backend_submission_input_from_value(&json!({
            "userAnswer": "answer",
            "action": "startGame",
            "result": "close",
        }))
        .expect("public submission fields should be accepted");
        assert_eq!(i16::from(input.action), 3);
        assert_eq!(input.result.as_deref(), Some("close"));

        let submission = crate::db::puzzle::BackendSubmissionShowData {
            id: 1,
            team_id: 2,
            user_id: 3,
            puzzle_id: 4,
            user_answer: "answer".to_string(),
            norm_answer: "answer".to_string(),
            saction: crate::model::game::RbJudgeAction::Milestone,
            sresult: Some("close".to_string()),
            real_answer: None,
            ignored: false,
            ctime_at: time::OffsetDateTime::UNIX_EPOCH,
        };
        let value = serde_json::to_value(&submission).expect("submission should serialize");
        assert_eq!(value["action"], 2);
        assert_eq!(value["result"], "close");
        assert!(value.get("createdAt").is_some());

        let decoded = backend_submission_from_value(&value)
            .expect("public submission should deserialize for solve");
        assert_eq!(i16::from(decoded.saction), 2);
        assert_eq!(decoded.sresult.as_deref(), Some("close"));
    }
}
