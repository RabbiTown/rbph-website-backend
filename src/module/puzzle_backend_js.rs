use std::cell::RefCell;

use boa_engine::{
    Context, JsError, JsNativeError, JsString, JsValue, Module, NativeFunction, Source, js_string,
    object::ObjectInitializer, property::Attribute,
};
use serde_json::{Map, Value};

use crate::{
    AppState, DbPool,
    db::{asset, puzzle_backend},
    error::RbInternalError,
    module::storage::LocalStorage,
};

#[derive(Clone)]
pub struct RuntimeContext {
    pub game_id: i32,
    pub method: String,
    pub puzzle_id: i32,
    pub team_id: i32,
    pub user_id: i32,
    pub api_name: String,
    pub query: Value,
    pub body: Value,
    pub puzzle_title: String,
    pub user_nickname: String,
    pub team_name: String,
}

#[derive(Clone)]
pub struct AssetRuntime {
    pub db: DbPool,
    pub storage: LocalStorage,
    pub max_read_bytes: u64,
}

const DEFAULT_MAX_ASSET_READ_BYTES: u64 = 5 * 1024 * 1024;

thread_local! {
    static RUNTIME_CONTEXT: RefCell<Option<RuntimeContext>> = const { RefCell::new(None) };
}

struct RuntimeContextGuard;

impl Drop for RuntimeContextGuard {
    fn drop(&mut self) {
        RUNTIME_CONTEXT.with(|slot| {
            *slot.borrow_mut() = None;
        });
    }
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

fn js_err(msg: impl Into<String>) -> boa_engine::JsError {
    JsNativeError::typ().with_message(msg.into()).into()
}

fn internal_err(msg: impl Into<String>) -> RbInternalError {
    RbInternalError::Other(msg.into())
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

fn js_string_arg(value: Option<JsString>, message: &str) -> Result<String, JsError> {
    value
        .map(|v| v.to_std_string_escaped())
        .ok_or_else(|| js_err(message))
}

fn block_on_db<T>(
    future: impl std::future::Future<Output = Result<T, RbInternalError>>,
) -> Result<T, RbInternalError> {
    futures::executor::block_on(future)
}

fn block_on_io<T>(
    future: impl std::future::Future<Output = Result<T, RbInternalError>>,
) -> Result<T, RbInternalError> {
    futures::executor::block_on(future)
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
    let file = block_on_db(asset::get_readable_file_by_object_key(
        &asset_runtime.db,
        runtime.game_id,
        runtime.puzzle_id,
        object_key,
        relative_path,
    ))
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
    block_on_io(asset_runtime.storage.read_object_file_limited(
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
        return Ok(action.into());
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
        .get("saction")
        .or_else(|| obj.get("action"))
        .map(submission_action_from_value)
        .transpose()?
        .unwrap_or(crate::model::game::RbJudgeAction::Correct);
    let sresult = obj
        .get("sresult")
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
        saction,
        sresult,
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
enum StoreCollectionScope {
    Team,
    Puzzle,
}

fn store_scope_from_schema(schema: &Value) -> Result<StoreCollectionScope, JsError> {
    match schema
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or("team")
    {
        "team" => Ok(StoreCollectionScope::Team),
        "puzzle" => Ok(StoreCollectionScope::Puzzle),
        _ => Err(js_err("$store collection scope must be team or puzzle")),
    }
}

fn configure_runtime_limits(context: &mut Context) {
    let limits = context.runtime_limits_mut();
    limits.set_loop_iteration_limit(100_000);
    limits.set_recursion_limit(128);
    limits.set_stack_size_limit(1024 * 4);
    limits.set_backtrace_limit(16);
}

pub fn register_ctx(
    context: &mut Context,
    app: actix_web::web::Data<AppState>,
    asset_runtime: AssetRuntime,
) -> Result<(), RbInternalError> {
    let db = app.db.clone();
    let kv_get_db = db.clone();
    let kv_set_db = db.clone();
    let kv_delete_db = db.clone();
    let kv_global_get_db = db.clone();
    let kv_global_set_db = db.clone();
    let kv_global_delete_db = db.clone();
    let store_db = db.clone();
    let currency_query_db = db.clone();
    let currency_cost_db = db.clone();
    let currency_query_db_for_query = currency_query_db.clone();
    let currency_query_db_for_add = currency_query_db.clone();
    let currency_cost_db_for_cost = currency_cost_db.clone();
    let backend_app = app.clone();
    let util_app = app.clone();
    let asset_list_runtime = asset_runtime.clone();
    let asset_read_text_runtime = asset_runtime.clone();
    let asset_read_json_runtime = asset_runtime.clone();
    let asset_read_bytes_runtime = asset_runtime;

    let kv_puzzle = ObjectInitializer::new(context)
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, context| {
                        let key = js_string_arg(
                            args.first().and_then(|value| value.as_string()),
                            "$kv.puzzle.get requires a key",
                        )?;
                        let runtime = with_runtime_context(|ctx| ctx.clone())
                            .map_err(|e| js_err(e.to_string()))?;
                        let value = block_on_db(puzzle_backend::get_kv(
                            &kv_global_get_db,
                            runtime.puzzle_id,
                            None,
                            &key,
                        ))
                        .map_err(|e| js_err(e.to_string()))?
                        .unwrap_or(Value::Null);
                        json_to_js(&value, context).map_err(|e| js_err(e.to_string()))
                    },
                    (),
                )
            },
            js_string!("get"),
            1,
        )
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, context| {
                        let key = js_string_arg(
                            args.first().and_then(|value| value.as_string()),
                            "$kv.puzzle.set requires a key",
                        )?;
                        let value = args.get(1).cloned().unwrap_or_else(JsValue::null);
                        let value =
                            js_to_json(&value, context).map_err(|e| js_err(e.to_string()))?;
                        let runtime = with_runtime_context(|ctx| ctx.clone())
                            .map_err(|e| js_err(e.to_string()))?;
                        let value = block_on_db(puzzle_backend::set_kv(
                            &kv_global_set_db,
                            runtime.puzzle_id,
                            None,
                            &key,
                            &value,
                        ))
                        .map_err(|e| js_err(e.to_string()))?;
                        json_to_js(&value, context).map_err(|e| js_err(e.to_string()))
                    },
                    (),
                )
            },
            js_string!("set"),
            2,
        )
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, _context| {
                        let key = js_string_arg(
                            args.first().and_then(|value| value.as_string()),
                            "$kv.puzzle.del requires a key",
                        )?;
                        let runtime = with_runtime_context(|ctx| ctx.clone())
                            .map_err(|e| js_err(e.to_string()))?;
                        let deleted = block_on_db(puzzle_backend::delete_kv(
                            &kv_global_delete_db,
                            runtime.puzzle_id,
                            None,
                            &key,
                        ))
                        .map_err(|e| js_err(e.to_string()))?;
                        Ok(JsValue::from(deleted))
                    },
                    (),
                )
            },
            js_string!("del"),
            1,
        )
        .build();

    let kv = ObjectInitializer::new(context)
        .property(js_string!("puzzle"), kv_puzzle, Attribute::all())
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, context| {
                        let key = js_string_arg(
                            args.first().and_then(|value| value.as_string()),
                            "$kv.get requires a key",
                        )?;
                        let runtime = with_runtime_context(|ctx| ctx.clone())
                            .map_err(|e| js_err(e.to_string()))?;
                        let value = block_on_db(puzzle_backend::get_kv(
                            &kv_get_db,
                            runtime.puzzle_id,
                            Some(runtime.team_id),
                            &key,
                        ))
                        .map_err(|e| js_err(e.to_string()))?
                        .unwrap_or(Value::Null);
                        json_to_js(&value, context).map_err(|e| js_err(e.to_string()))
                    },
                    (),
                )
            },
            js_string!("get"),
            1,
        )
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, context| {
                        let key = js_string_arg(
                            args.first().and_then(|value| value.as_string()),
                            "$kv.set requires a key",
                        )?;
                        let value = args.get(1).cloned().unwrap_or_else(JsValue::null);
                        let value =
                            js_to_json(&value, context).map_err(|e| js_err(e.to_string()))?;
                        let runtime = with_runtime_context(|ctx| ctx.clone())
                            .map_err(|e| js_err(e.to_string()))?;
                        let value = block_on_db(puzzle_backend::set_kv(
                            &kv_set_db,
                            runtime.puzzle_id,
                            Some(runtime.team_id),
                            &key,
                            &value,
                        ))
                        .map_err(|e| js_err(e.to_string()))?;
                        json_to_js(&value, context).map_err(|e| js_err(e.to_string()))
                    },
                    (),
                )
            },
            js_string!("set"),
            2,
        )
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, _context| {
                        let key = js_string_arg(
                            args.first().and_then(|value| value.as_string()),
                            "$kv.del requires a key",
                        )?;
                        let runtime = with_runtime_context(|ctx| ctx.clone())
                            .map_err(|e| js_err(e.to_string()))?;
                        let deleted = block_on_db(puzzle_backend::delete_kv(
                            &kv_delete_db,
                            runtime.puzzle_id,
                            Some(runtime.team_id),
                            &key,
                        ))
                        .map_err(|e| js_err(e.to_string()))?;
                        Ok(JsValue::from(deleted))
                    },
                    (),
                )
            },
            js_string!("del"),
            1,
        )
        .build();

    let util = ObjectInitializer::new(context)
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, context| {
                        let team_id = args
                            .first()
                            .and_then(|value| value.as_number())
                            .ok_or_else(|| js_err("$util.queryCurrency requires team id"))?
                            as i32;
                        let currency_id = args
                            .get(1)
                            .and_then(|value| value.as_number())
                            .map(|v| v as i32);
                        match currency_id {
                            Some(currency_id) => {
                                let row = block_on_db(crate::db::team::get_currency_info_one(
                                    &currency_query_db_for_query,
                                    team_id,
                                    currency_id,
                                ))
                                .map_err(|e| js_err(e.to_string()))?;
                                let json =
                                    serde_json::to_value(row).map_err(|e| js_err(e.to_string()))?;
                                json_to_js(&json, context).map_err(|e| js_err(e.to_string()))
                            }
                            None => {
                                let rows = block_on_db(crate::db::team::get_currency_info(
                                    &currency_query_db_for_query,
                                    team_id,
                                ))
                                .map_err(|e| js_err(e.to_string()))?;
                                let json = serde_json::to_value(rows)
                                    .map_err(|e| js_err(e.to_string()))?;
                                json_to_js(&json, context).map_err(|e| js_err(e.to_string()))
                            }
                        }
                    },
                    (),
                )
            },
            js_string!("queryCurrency"),
            2,
        )
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, _context| {
                        let team_id = args
                            .first()
                            .and_then(|value| value.as_number())
                            .ok_or_else(|| js_err("$util.costCurrency requires team id"))?
                            as i32;
                        let currency_id = args
                            .get(1)
                            .and_then(|value| value.as_number())
                            .ok_or_else(|| js_err("$util.costCurrency requires currency id"))?
                            as i32;
                        let amount = args
                            .get(2)
                            .and_then(|value| value.as_number())
                            .ok_or_else(|| js_err("$util.costCurrency requires amount"))?
                            as i32;

                        let updated = block_on_db(crate::db::team::cost_currency(
                            &currency_cost_db_for_cost,
                            team_id,
                            currency_id,
                            -amount,
                        ))
                        .map_err(|e| js_err(e.to_string()))?;
                        Ok(JsValue::from(updated))
                    },
                    (),
                )
            },
            js_string!("costCurrency"),
            3,
        )
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, _context| {
                        let team_id = args
                            .first()
                            .and_then(|value| value.as_number())
                            .ok_or_else(|| js_err("$util.addCurrency requires team id"))?
                            as i32;
                        let currency_id = args
                            .get(1)
                            .and_then(|value| value.as_number())
                            .ok_or_else(|| js_err("$util.addCurrency requires currency id"))?
                            as i32;
                        let amount = args
                            .get(2)
                            .and_then(|value| value.as_number())
                            .ok_or_else(|| js_err("$util.addCurrency requires amount"))?
                            as i32;

                        let updated = block_on_db(crate::db::team::add_currency(
                            &currency_query_db_for_add,
                            team_id,
                            currency_id,
                            amount,
                        ))
                        .map_err(|e| js_err(e.to_string()))?;
                        Ok(match updated {
                            Some(delta) => JsValue::from(delta),
                            None => JsValue::null(),
                        })
                    },
                    (),
                )
            },
            js_string!("addCurrency"),
            3,
        )
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, context| {
                        let input_value = args
                            .first()
                            .ok_or_else(|| js_err("$util.addSubmission requires an object"))?;
                        let input_json =
                            js_to_json(input_value, context).map_err(|e| js_err(e.to_string()))?;
                        let input = backend_submission_input_from_value(&input_json)
                            .map_err(|e| js_err(e.to_string()))?;
                        let runtime = with_runtime_context(|ctx| ctx.clone())
                            .map_err(|e| js_err(e.to_string()))?;
                        let row =
                            block_on_db(crate::db::puzzle::add_backend_submission_and_invalidate(
                                backend_app.as_ref(),
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
            js_string!("addSubmission"),
            1,
        )
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, context| {
                        let submission_value = args
                            .first()
                            .ok_or_else(|| js_err("$util.solvePuzzle requires a submission"))?;
                        let submission_json = js_to_json(submission_value, context)
                            .map_err(|e| js_err(e.to_string()))?;
                        let submission = backend_submission_from_value(&submission_json)
                            .map_err(|e| js_err(e.to_string()))?;
                        let runtime = with_runtime_context(|ctx| ctx.clone())
                            .map_err(|e| js_err(e.to_string()))?;
                        let solved =
                            block_on_db(crate::db::puzzle::solve_backend_puzzle_with_submission(
                                util_app.as_ref(),
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
            js_string!("solvePuzzle"),
            1,
        )
        .build();

    let store = ObjectInitializer::new(context)
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, context| {
                        let collection = js_string_arg(
                            args.first().and_then(|value| value.as_string()),
                            "$store.collection requires a collection name",
                        )?;
                        validate_store_name(
                            &collection,
                            "$store collection name must be 1-64 chars using letters, numbers, _, -, or .",
                        )?;
                        let schema = args
                            .get(1)
                            .map(|value| js_to_json(value, context))
                            .transpose()
                            .map_err(|e| js_err(e.to_string()))?
                            .unwrap_or(Value::Null);
                        let indexes = schema
                            .get("indexes")
                            .and_then(Value::as_object)
                            .cloned()
                            .unwrap_or_default();
                        let scope = store_scope_from_schema(&schema)?;

                        let insert_db = store_db.clone();
                        let get_db = store_db.clone();
                        let list_db = store_db.clone();
                        let insert_collection = collection.clone();
                        let get_collection = collection.clone();
                        let list_collection = collection;
                        let insert_indexes = indexes.clone();
                        let list_indexes = indexes;
                        let insert_scope = scope;

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
                                        let (team_id, user_id) = match insert_scope {
                                            StoreCollectionScope::Team => {
                                                (Some(runtime.team_id), Some(runtime.user_id))
                                            }
                                            StoreCollectionScope::Puzzle => (None, None),
                                        };
                                        let doc = block_on_db(puzzle_backend::insert_store_doc(
                                            &insert_db,
                                            runtime.puzzle_id,
                                            &insert_collection,
                                            team_id,
                                            user_id,
                                            &value,
                                            &indexes,
                                        ))
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
                                        let doc = block_on_db(puzzle_backend::get_store_doc(
                                            &get_db,
                                            runtime.puzzle_id,
                                            &get_collection,
                                            doc_id,
                                        ))
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
                                        let docs = block_on_db(puzzle_backend::list_store_docs(
                                            &list_db,
                                            runtime.puzzle_id,
                                            &list_collection,
                                            &options,
                                        ))
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
            2,
        )
        .build();

    let asset = ObjectInitializer::new(context)
        .function(
            unsafe {
                NativeFunction::from_closure_with_captures(
                    move |_, args, _, context| {
                        let object_key = js_string_arg(
                            args.first().and_then(|value| value.as_string()),
                            "$asset.list requires an object key",
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
                            "$asset.readText requires an object key",
                        )?;
                        let relative_path = js_asset_path_arg(
                            args.get(1).and_then(|value| value.as_string()),
                            "$asset.readText requires a relative path",
                        )?;
                        let bytes = read_asset_bytes(
                            &asset_read_text_runtime,
                            &object_key,
                            &relative_path,
                        )?;
                        let text = String::from_utf8(bytes)
                            .map_err(|_| js_err("$asset.readText requires UTF-8 content"))?;
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
                            "$asset.readJson requires an object key",
                        )?;
                        let relative_path = js_asset_path_arg(
                            args.get(1).and_then(|value| value.as_string()),
                            "$asset.readJson requires a relative path",
                        )?;
                        let bytes = read_asset_bytes(
                            &asset_read_json_runtime,
                            &object_key,
                            &relative_path,
                        )?;
                        let json: Value = serde_json::from_slice(&bytes)
                            .map_err(|e| js_err(format!("$asset.readJson failed: {e}")))?;
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
                            "$asset.readBytes requires an object key",
                        )?;
                        let relative_path = js_asset_path_arg(
                            args.get(1).and_then(|value| value.as_string()),
                            "$asset.readBytes requires a relative path",
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
        .build();

    context
        .register_global_property(js_string!("$util"), util, Attribute::all())
        .map_err(|e| internal_err(e.to_string()))?;
    context
        .register_global_property(js_string!("$kv"), kv, Attribute::all())
        .map_err(|e| internal_err(e.to_string()))?;
    context
        .register_global_property(js_string!("$store"), store, Attribute::all())
        .map_err(|e| internal_err(e.to_string()))?;
    context
        .register_global_property(js_string!("$asset"), asset, Attribute::all())
        .map_err(|e| internal_err(e.to_string()))?;
    Ok(())
}

pub async fn execute_api(
    app: actix_web::web::Data<AppState>,
    backend: crate::db::puzzle_backend::PuzzleBackend,
    api_name: String,
    runtime: RuntimeContext,
) -> Result<Value, RbInternalError> {
    tokio::task::spawn_blocking(move || {
        RUNTIME_CONTEXT.with(|slot| {
            *slot.borrow_mut() = Some(runtime);
        });
        let _guard = RuntimeContextGuard;

        let mut context = Context::default();
        configure_runtime_limits(&mut context);
        register_ctx(
            &mut context,
            app.clone(),
            AssetRuntime {
                db: app.db.clone(),
                storage: app.storage.clone(),
                max_read_bytes: DEFAULT_MAX_ASSET_READ_BYTES,
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
            .get(js_string!(api_name.as_str()), &mut context)
            .map_err(|e| internal_err(e.to_string()))?;
        let function = value
            .as_function()
            .ok_or_else(|| RbInternalError::Other("export is not a function".to_string()))?;

        let ctx_arg = build_ctx_arg(&mut context)?;
        let result = function
            .call(&JsValue::undefined(), &[ctx_arg], &mut context)
            .map_err(|e| internal_err(e.to_string()))?;
        let result = if let Some(promise) = result.as_promise() {
            context
                .run_jobs()
                .map_err(|e| internal_err(e.to_string()))?;
            match promise.state() {
                boa_engine::builtins::promise::PromiseState::Fulfilled(value) => value.clone(),
                boa_engine::builtins::promise::PromiseState::Rejected(reason) => {
                    return Err(internal_err(format!("api rejected: {}", reason.display())));
                }
                boa_engine::builtins::promise::PromiseState::Pending => {
                    return Err(internal_err("api promise is still pending"));
                }
            }
        } else {
            result
        };
        let json = js_to_json(&result, &mut context)?;

        Ok::<_, RbInternalError>(json)
    })
    .await
    .map_err(|e| internal_err(e.to_string()))?
}
