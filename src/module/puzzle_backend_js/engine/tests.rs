use std::{sync::Arc, time::Duration};

use serde_json::{Value, json};

#[cfg(feature = "v8-engine")]
use super::v8::V8Engine;
use super::{EngineRequest, ExecutionKind, JsEngine, ResultMode, boa::BoaEngine};
use crate::module::puzzle_backend_js::{
    host::HostBridge,
    protocol::{HOST_PROTOCOL_VERSION, HostError, HostRequest, HostValue},
};

struct NullHost;

impl HostBridge for NullHost {
    fn call(&self, _request: HostRequest) -> Result<HostValue, HostError> {
        Ok(HostValue::Undefined)
    }
}

struct FailingHost;

impl HostBridge for FailingHost {
    fn call(&self, _request: HostRequest) -> Result<HostValue, HostError> {
        Err(HostError::invalid("host rejected the call"))
    }
}

fn engines() -> Vec<(&'static str, &'static dyn JsEngine)> {
    static BOA: BoaEngine = BoaEngine;
    #[cfg(feature = "v8-engine")]
    static V8: V8Engine = V8Engine;

    let engines: Vec<(&str, &dyn JsEngine)> = vec![("boa", &BOA)];
    #[cfg(feature = "v8-engine")]
    let engines = {
        let mut engines = engines;
        engines.push(("v8", &V8));
        engines
    };
    engines
}

fn request(source: &str, argument: Value, result_mode: ResultMode) -> EngineRequest {
    EngineRequest {
        source: source.to_string(),
        function_name: "run".to_string(),
        execution_kind: ExecutionKind::Api,
        argument,
        bootstrap_metadata: json!({
            "protocolVersion": HOST_PROTOCOL_VERSION,
            "gameId": 1,
            "team": { "id": 2, "name": "Team" },
            "puzzle": { "id": 3, "title": "Puzzle" },
        }),
        result_mode,
        wall_time_limit: Duration::from_secs(5),
    }
}

#[test]
fn engines_return_json_and_resolve_fulfilled_promises() {
    for (name, engine) in engines() {
        let result = engine
            .execute(
                request(
                    "export async function run(value) { return { answer: value + 1 }; }",
                    json!(41),
                    ResultMode::JsonRequired,
                ),
                Arc::new(NullHost),
            )
            .unwrap_or_else(|error| panic!("{name} failed: {error}"));
        assert_eq!(result, HostValue::Json(json!({ "answer": 42 })), "{name}");
    }
}

#[test]
fn engines_preserve_undefined_result_modes() {
    for (name, engine) in engines() {
        let required = engine.execute(
            request(
                "export function run() {}",
                Value::Null,
                ResultMode::JsonRequired,
            ),
            Arc::new(NullHost),
        );
        assert!(required.is_err(), "{name}");

        let allowed = engine
            .execute(
                request(
                    "export function run() {}",
                    Value::Null,
                    ResultMode::UndefinedAllowed,
                ),
                Arc::new(NullHost),
            )
            .unwrap_or_else(|error| panic!("{name} failed: {error}"));
        assert_eq!(allowed, HostValue::Undefined, "{name}");
    }
}

#[test]
fn engines_use_a_fresh_context_for_every_execution() {
    for (name, engine) in engines() {
        for _ in 0..2 {
            let result = engine
                .execute(
                    request(
                        "globalThis.__counter = (globalThis.__counter ?? 0) + 1; export function run() { return globalThis.__counter; }",
                        Value::Null,
                        ResultMode::JsonRequired,
                    ),
                    Arc::new(NullHost),
                )
                .unwrap_or_else(|error| panic!("{name} failed: {error}"));
            assert_eq!(result, HostValue::Json(json!(1)), "{name}");
        }
    }
}

#[test]
fn engines_preserve_host_error_messages() {
    for (name, engine) in engines() {
        let error = engine
            .execute(
                request(
                    "export function run() { return $this.event.emit('ready'); }",
                    Value::Null,
                    ResultMode::Ignored,
                ),
                Arc::new(FailingHost),
            )
            .expect_err("host error should escape JavaScript");
        assert!(
            error.to_string().contains("host rejected the call"),
            "{name}: {error}"
        );
    }
}

#[test]
fn engines_reject_promises_that_never_settle() {
    for (name, engine) in engines() {
        let error = engine
            .execute(
                request(
                    "export function run() { return new Promise(() => {}); }",
                    Value::Null,
                    ResultMode::JsonRequired,
                ),
                Arc::new(NullHost),
            )
            .expect_err("pending promise must fail");
        assert!(
            error.to_string().contains("promise is still pending"),
            "{name}: {error}"
        );
    }
}

#[cfg(feature = "v8-engine")]
#[test]
fn v8_rejects_module_imports() {
    for source in [
        "import value from './dependency.js'; export function run() { return value; }",
        "export async function run() { return import('./dependency.js'); }",
    ] {
        let error = V8Engine
            .execute(
                request(source, Value::Null, ResultMode::JsonRequired),
                Arc::new(NullHost),
            )
            .expect_err("imports must not be loaded");
        assert!(
            error
                .to_string()
                .contains("module imports are not supported")
        );
    }
}

#[cfg(feature = "v8-engine")]
#[test]
fn v8_terminates_pure_javascript_after_the_deadline() {
    let mut request = request(
        "export function run() { while (true) {} }",
        Value::Null,
        ResultMode::Ignored,
    );
    request.wall_time_limit = Duration::from_millis(50);
    let error = V8Engine
        .execute(request, Arc::new(NullHost))
        .expect_err("infinite loop must time out");
    assert!(
        error
            .to_string()
            .contains("backend function execution timed out")
    );
}

#[cfg(feature = "v8-engine")]
#[test]
fn v8_disables_wasm_code_generation() {
    let error = V8Engine
        .execute(
            request(
                "export function run() { return new WebAssembly.Module(new Uint8Array([0, 97, 115, 109, 1, 0, 0, 0])); }",
                Value::Null,
                ResultMode::Ignored,
            ),
            Arc::new(NullHost),
        )
        .expect_err("WebAssembly compilation must be disabled");
    assert!(error.to_string().contains("WebAssembly"));
}
