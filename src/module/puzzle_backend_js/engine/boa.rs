use std::sync::Arc;

use boa_engine::{
    Context, JsNativeError, JsValue, Module, NativeFunction, Source,
    builtins::promise::PromiseState, js_string, property::Attribute,
};
use serde_json::Value;

use crate::error::RbInternalError;

use super::{EngineRequest, ExecutionKind, JsEngine, ResultMode, internal_err};
use crate::module::puzzle_backend_js::{
    host::HostBridge,
    protocol::{HostCall, HostRequest, HostValue},
};

const BOOTSTRAP_SOURCE: &str = include_str!("../bootstrap.js");
const JUDGE_LOOP_ITERATION_LIMIT: u64 = 50_000;

fn js_error(message: impl Into<String>) -> boa_engine::JsError {
    JsNativeError::typ().with_message(message.into()).into()
}

fn json_to_js(value: &Value, context: &mut Context) -> Result<JsValue, RbInternalError> {
    JsValue::from_json(value, context).map_err(|error| internal_err(error.to_string()))
}

fn js_to_json(value: &JsValue, context: &mut Context) -> Result<Value, RbInternalError> {
    value
        .to_json(context)
        .map_err(|error| internal_err(error.to_string()))?
        .ok_or_else(|| internal_err("value is undefined"))
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
        .map_err(|error| internal_err(error.to_string()))?;
    match promise.state() {
        PromiseState::Fulfilled(value) => Ok(value.clone()),
        PromiseState::Rejected(reason) => {
            Err(internal_err(format!("api rejected: {}", reason.display())))
        }
        PromiseState::Pending => Err(internal_err("api promise is still pending")),
    }
}

fn configure_runtime_limits(context: &mut Context, execution_kind: ExecutionKind) {
    let limits = context.runtime_limits_mut();
    limits.set_loop_iteration_limit(match execution_kind {
        ExecutionKind::Judge => JUDGE_LOOP_ITERATION_LIMIT,
        ExecutionKind::Api | ExecutionKind::HintPurchase => 100_000,
    });
    limits.set_recursion_limit(128);
    limits.set_stack_size_limit(1024 * 4);
    limits.set_backtrace_limit(16);
}

fn register_host_bridge(
    context: &mut Context,
    host: Arc<dyn HostBridge>,
) -> Result<(), RbInternalError> {
    let function = unsafe {
        NativeFunction::from_closure_with_captures(
            move |_, args, _, context| {
                let operation = args
                    .first()
                    .and_then(JsValue::as_string)
                    .map(|value| value.to_std_string_escaped())
                    .ok_or_else(|| js_error("host operation must be a string"))?;
                let payload = args.get(1).cloned().unwrap_or_else(JsValue::null);
                let payload = payload
                    .to_json(context)
                    .map_err(|error| js_error(error.to_string()))?
                    .ok_or_else(|| js_error("value is undefined"))?;
                let mut payload = payload
                    .as_object()
                    .cloned()
                    .ok_or_else(|| js_error("host payload must be an object"))?;
                payload.insert("operation".to_string(), Value::String(operation));
                let call = serde_json::from_value::<HostCall>(Value::Object(payload))
                    .map_err(|error| js_error(error.to_string()))?;
                match host
                    .call(HostRequest::current(call))
                    .map_err(|error| js_error(error.message))?
                {
                    HostValue::Json(value) => JsValue::from_json(&value, context)
                        .map_err(|error| js_error(error.to_string())),
                    HostValue::Undefined => Ok(JsValue::undefined()),
                }
            },
            (),
        )
    };
    context
        .register_global_builtin_callable(js_string!("__rbph_native_call"), 2, function)
        .map_err(|error| internal_err(error.to_string()))
}

fn install_bootstrap(
    context: &mut Context,
    metadata: &Value,
    host: Arc<dyn HostBridge>,
) -> Result<(), RbInternalError> {
    register_host_bridge(context, host)?;
    let metadata = json_to_js(metadata, context)?;
    context
        .register_global_property(
            js_string!("__rbph_bootstrap_metadata"),
            metadata,
            Attribute::all(),
        )
        .map_err(|error| internal_err(error.to_string()))?;
    context
        .eval(Source::from_bytes(BOOTSTRAP_SOURCE))
        .map_err(|error| internal_err(error.to_string()))?;
    Ok(())
}

pub(super) struct BoaEngine;

impl JsEngine for BoaEngine {
    fn execute(
        &self,
        request: EngineRequest,
        host: Arc<dyn HostBridge>,
    ) -> Result<HostValue, RbInternalError> {
        let _wall_time_limit = request.wall_time_limit;
        let mut context = Context::default();
        configure_runtime_limits(&mut context, request.execution_kind);
        install_bootstrap(&mut context, &request.bootstrap_metadata, host)?;

        let module = Module::parse(Source::from_bytes(&request.source), None, &mut context)
            .map_err(|error| internal_err(error.to_string()))?;
        let promise = module.load_link_evaluate(&mut context);
        context
            .run_jobs()
            .map_err(|error| internal_err(error.to_string()))?;
        if let Some(error) = promise.state().as_rejected() {
            return Err(internal_err(format!(
                "module rejected: {}",
                error.display()
            )));
        }

        let namespace = module.namespace(&mut context);
        let value = namespace
            .get(js_string!(request.function_name.as_str()), &mut context)
            .map_err(|error| internal_err(error.to_string()))?;
        let function = value
            .as_function()
            .ok_or_else(|| internal_err("export is not a function"))?;
        let argument = json_to_js(&request.argument, &mut context)?;
        let result = function
            .call(&JsValue::undefined(), &[argument], &mut context)
            .map_err(|error| internal_err(error.to_string()))?;
        let result = resolve_promise_value(result, &mut context)?;

        match request.result_mode {
            ResultMode::JsonRequired => Ok(HostValue::Json(js_to_json(&result, &mut context)?)),
            ResultMode::UndefinedAllowed => match result
                .to_json(&mut context)
                .map_err(|error| internal_err(error.to_string()))?
            {
                Some(value) => Ok(HostValue::Json(value)),
                None => Ok(HostValue::Undefined),
            },
            ResultMode::Ignored => Ok(HostValue::Undefined),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::module::puzzle_backend_js::protocol::{HOST_PROTOCOL_VERSION, HostError};

    #[derive(Default)]
    struct RecordingHost {
        calls: Mutex<Vec<HostCall>>,
    }

    impl HostBridge for RecordingHost {
        fn call(&self, request: HostRequest) -> Result<HostValue, HostError> {
            assert_eq!(request.protocol_version, HOST_PROTOCOL_VERSION);
            self.calls.lock().expect("calls lock").push(request.call);
            Ok(HostValue::Json(json!({ "ok": true })))
        }
    }

    fn metadata() -> Value {
        json!({
            "protocolVersion": HOST_PROTOCOL_VERSION,
            "gameId": 1,
            "team": { "id": 2, "name": "Team" },
            "puzzle": { "id": 3, "title": "Puzzle" },
        })
    }

    fn request(source: &str, mode: ResultMode) -> EngineRequest {
        EngineRequest {
            source: source.to_string(),
            function_name: "run".to_string(),
            execution_kind: ExecutionKind::Api,
            argument: Value::Null,
            bootstrap_metadata: metadata(),
            result_mode: mode,
            wall_time_limit: std::time::Duration::from_secs(5),
        }
    }

    #[test]
    fn bootstrap_exposes_compatible_globals_and_function_lengths() {
        let host = Arc::new(RecordingHost::default());
        let value = BoaEngine
            .execute(
                request(
                    r#"export function run() {
                        const descriptor = Object.getOwnPropertyDescriptor(globalThis, "$this");
                        return {
                            ids: [$game.id, $team.id, $puzzle.id, $this.team === $team],
                            names: [$team.name, $puzzle.title],
                            lengths: [
                                $game.kv.get.length,
                                $team.kv.get.length,
                                $this.kv.compareAndSet.length,
                                $game.store.collection.length,
                                $team.currency.update.length,
                                $puzzle.assets.readBytes.length,
                                $this.event.emit.length,
                                console.log.length
                            ],
                            nativeRemoved: !("__rbph_native_call" in globalThis),
                            descriptor: {
                                writable: descriptor.writable,
                                enumerable: descriptor.enumerable,
                                configurable: descriptor.configurable,
                            },
                        };
                    }"#,
                    ResultMode::JsonRequired,
                ),
                host,
            )
            .expect("script should execute");
        assert_eq!(
            value,
            HostValue::Json(json!({
                "ids": [1, 2, 3, true],
                "names": ["Team", "Puzzle"],
                "lengths": [2, 2, 5, 3, 4, 2, 2, 1],
                "nativeRemoved": true,
                "descriptor": {
                    "writable": true,
                    "enumerable": true,
                    "configurable": true,
                },
            }))
        );
    }

    #[test]
    fn bootstrap_routes_calls_through_single_host_bridge() {
        let host = Arc::new(RecordingHost::default());
        BoaEngine
            .execute(
                request(
                    r#"export function run() {
                        $this.kv.set("answer", { ready: true }, { ttl: 30000 });
                        console.warn("value", 42);
                        $this.event.emit("ready");
                        return null;
                    }"#,
                    ResultMode::JsonRequired,
                ),
                host.clone(),
            )
            .expect("script should execute");
        let calls = host.calls.lock().expect("calls lock");
        assert!(matches!(calls[0], HostCall::KvSet { .. }));
        assert!(matches!(calls[1], HostCall::ConsoleWrite { .. }));
        assert!(matches!(calls[2], HostCall::EventEmit { .. }));
    }

    #[test]
    fn bootstrap_routes_every_public_host_operation() {
        let host = Arc::new(RecordingHost::default());
        BoaEngine
            .execute(
                request(
                    r#"export function run() {
                        const scope = { type: "teamPuzzle", teamId: 2, puzzleId: 3 };
                        $game.kv.get(scope, "key");
                        $game.kv.getEntry(scope, "key");
                        $game.kv.set(scope, "key", 1);
                        $game.kv.increment(scope, "key");
                        $game.kv.setIfAbsent(scope, "key", 1);
                        $game.kv.compareAndSet(scope, "key", "1", 2);
                        $game.kv.delete(scope, "key");
                        const collection = $team.store.collection("scores", { indexes: { score: "number" } });
                        collection.insert({ score: 10 });
                        collection.get(1);
                        collection.list({ where: { score: { eq: 10 } }, order: "asc" });
                        $team.currency.query();
                        $team.currency.cost("coin", 1);
                        $team.currency.add("coin", 2, "bonus");
                        $team.currency.update("coin", { amount: 3, teamGrowth: 1, hidden: false });
                        $puzzle.assets.list("guide");
                        $puzzle.assets.readText("guide", "readme.txt");
                        $puzzle.assets.readJson("guide", "data.json");
                        $puzzle.assets.readBytes("guide", "data.bin");
                        $this.submission.add({ userAnswer: "answer" });
                        $this.solve({ id: 1 });
                        $this.event.emit("ready", { value: 1 });
                        console.info("done");
                        return null;
                    }"#,
                    ResultMode::JsonRequired,
                ),
                host.clone(),
            )
            .expect("all public host calls should route");

        let calls = host.calls.lock().expect("calls lock");
        assert_eq!(calls.len(), 22);
        assert!(matches!(calls[0], HostCall::KvGet { .. }));
        assert!(matches!(calls[1], HostCall::KvGetEntry { .. }));
        assert!(matches!(calls[2], HostCall::KvSet { .. }));
        assert!(matches!(calls[3], HostCall::KvIncrement { .. }));
        assert!(matches!(calls[4], HostCall::KvSetIfAbsent { .. }));
        assert!(matches!(calls[5], HostCall::KvCompareAndSet { .. }));
        assert!(matches!(calls[6], HostCall::KvDelete { .. }));
        assert!(matches!(calls[7], HostCall::StoreInsert { .. }));
        assert!(matches!(calls[8], HostCall::StoreGet { .. }));
        assert!(matches!(calls[9], HostCall::StoreList { .. }));
        assert!(matches!(calls[10], HostCall::CurrencyQuery { .. }));
        assert!(matches!(calls[11], HostCall::CurrencyCost { .. }));
        assert!(matches!(calls[12], HostCall::CurrencyAdd { .. }));
        assert!(matches!(calls[13], HostCall::CurrencyUpdate { .. }));
        assert!(matches!(calls[14], HostCall::AssetList { .. }));
        assert!(matches!(calls[15], HostCall::AssetReadText { .. }));
        assert!(matches!(calls[16], HostCall::AssetReadJson { .. }));
        assert!(matches!(calls[17], HostCall::AssetReadBytes { .. }));
        assert!(matches!(calls[18], HostCall::SubmissionAdd { .. }));
        assert!(matches!(calls[19], HostCall::PuzzleSolve { .. }));
        assert!(matches!(calls[20], HostCall::EventEmit { .. }));
        assert!(matches!(calls[21], HostCall::ConsoleWrite { .. }));
    }

    #[test]
    fn bootstrap_preserves_kv_defaults_and_event_null_payload() {
        let host = Arc::new(RecordingHost::default());
        BoaEngine
            .execute(
                request(
                    r#"export function run() {
                        $this.kv.increment("counter");
                        $this.kv.set("preserved", null);
                        $this.kv.setIfAbsent("permanent", null, { ttl: null });
                        $this.event.emit("ready");
                        return null;
                    }"#,
                    ResultMode::JsonRequired,
                ),
                host.clone(),
            )
            .expect("defaults should route");

        let calls = host.calls.lock().expect("calls lock");
        assert!(matches!(
            &calls[0],
            HostCall::KvIncrement {
                amount,
                expiry: crate::module::puzzle_backend_js::protocol::HostKvExpiry::Preserve,
                ..
            } if *amount == 1.0
        ));
        assert!(matches!(
            &calls[1],
            HostCall::KvSet {
                value: Value::Null,
                expiry: crate::module::puzzle_backend_js::protocol::HostKvExpiry::Preserve,
                ..
            }
        ));
        assert!(matches!(
            &calls[2],
            HostCall::KvSetIfAbsent {
                expiry: crate::module::puzzle_backend_js::protocol::HostKvExpiry::Permanent,
                ..
            }
        ));
        assert!(matches!(
            &calls[3],
            HostCall::EventEmit {
                payload: Value::Null,
                ..
            }
        ));
    }

    #[test]
    fn bootstrap_preserves_undefined_and_argument_errors() {
        for (body, message) in [
            ("$this.kv.set('key', undefined)", "value is undefined"),
            (
                "$this.kv.increment('key', null)",
                "$this.kv.increment amount must be a finite number",
            ),
            (
                "$this.store.collection('items', undefined)",
                "value is undefined",
            ),
            (
                "$this.store.collection('items').list(undefined)",
                "value is undefined",
            ),
            (
                "$game.kv.get({ type: 'team' }, 'key')",
                "team scope requires teamId",
            ),
        ] {
            let source = format!("export function run() {{ {body}; }}");
            let error = BoaEngine
                .execute(
                    request(&source, ResultMode::JsonRequired),
                    Arc::new(RecordingHost::default()),
                )
                .expect_err("invalid arguments should fail");
            assert!(
                error.to_string().contains(message),
                "expected `{message}` in `{error}`"
            );
        }
    }

    #[test]
    fn result_modes_preserve_undefined_behavior() {
        let host = Arc::new(RecordingHost::default());
        assert!(
            BoaEngine
                .execute(
                    request("export function run() {}", ResultMode::JsonRequired),
                    host.clone(),
                )
                .is_err()
        );
        assert_eq!(
            BoaEngine
                .execute(
                    request("export function run() {}", ResultMode::UndefinedAllowed),
                    host,
                )
                .expect("undefined should be allowed"),
            HostValue::Undefined
        );
    }
}
