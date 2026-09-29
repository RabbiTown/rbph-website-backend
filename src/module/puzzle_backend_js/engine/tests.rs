use std::{sync::Arc, time::Duration};

use serde_json::{Value, json};

#[cfg(feature = "v8-engine")]
use super::v8::V8Engine;
use super::{EngineRequest, ExecutionKind, JsEngine, ResultMode, boa::BoaEngine};
use crate::module::puzzle_backend_js::{
    host::HostBridge,
    protocol::{HOST_PROTOCOL_VERSION, HostCall, HostError, HostRequest, HostValue},
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

#[derive(Default)]
struct StorageRecordingHost {
    calls: std::sync::Mutex<Vec<HostCall>>,
}

impl HostBridge for StorageRecordingHost {
    fn call(&self, request: HostRequest) -> Result<HostValue, HostError> {
        let result = match &request.call {
            HostCall::KvGetEntries { keys, .. } => json!(keys.iter().map(|key| {
                if key == "missing" { Value::Null } else { json!({ "value": key, "version": "9007199254740993", "expiresAt": null }) }
            }).collect::<Vec<_>>()),
            HostCall::StorageCommit { .. } => json!({"applied": true, "entries": [], "documents": [], "serverTime": "2026-09-28T00:00:00Z"}),
            _ => Value::Null,
        };
        self.calls.lock().unwrap().push(request.call);
        Ok(HostValue::Json(result))
    }
}

#[test]
fn engines_batch_reads_preserve_order_and_use_one_host_call() {
    for (name, engine) in engines() {
        let host = Arc::new(StorageRecordingHost::default());
        let result = engine
            .execute(
                request(
                    r#"export function run() {
            const empty = $this.kv.getEntries([]);
            const entries = $this.kv.getEntries(['b', 'missing', 'b', 'a']);
            $game.kv.getEntries({type: 'global'}, ['c']);
            return {empty, entries};
        }"#,
                    Value::Null,
                    ResultMode::JsonRequired,
                ),
                host.clone(),
            )
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let HostValue::Json(result) = result else {
            panic!("JSON required");
        };
        assert_eq!(result["empty"], json!([]));
        assert_eq!(result["entries"][0], result["entries"][2]);
        assert_eq!(result["entries"][0]["version"], "9007199254740993");
        assert!(result["entries"][1].is_null());
        let calls = host.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert!(matches!(
            &calls[0],
            HostCall::KvGetEntries {
                scope: super::super::protocol::HostScope::TeamPuzzle {
                    team_id: 2,
                    puzzle_id: 3
                },
                ..
            }
        ));
    }
}

#[test]
fn engines_commit_encodes_conditions_expiry_and_all_scopes() {
    for (name, engine) in engines() {
        let host = Arc::new(StorageRecordingHost::default());
        engine.execute(request(r#"export function run() {
            const batch = {writes: [{key:'a', expectedVersion:null, value:{x:1}}],
                inserts:[{collection:'attempts', schema:{indexes:{puzzleId:'string'}}, value:{puzzleId:'a'}}]};
            $this.commit(batch); $team.commit({writes:[], inserts:batch.inserts}); $puzzle.commit(batch);
            $game.commit({type:'global'}, {writes:[{key:'b',expectedVersion:'9007199254740993',value:null}]});
            return true;
        }"#, Value::Null, ResultMode::JsonRequired), host.clone()).unwrap_or_else(|error| panic!("{name}: {error}"));
        let calls = host.calls.lock().unwrap();
        assert_eq!(calls.len(), 4);
        let HostCall::StorageCommit {
            writes, inserts, ..
        } = &calls[0]
        else {
            panic!("commit required");
        };
        assert!(writes[0].expected_version.is_none());
        assert_eq!(
            writes[0].expiry,
            super::super::protocol::HostKvExpiry::Permanent
        );
        assert_eq!(inserts.len(), 1);
        let HostCall::StorageCommit {
            writes, inserts, ..
        } = &calls[1]
        else {
            panic!("commit required");
        };
        assert!(writes.is_empty());
        assert_eq!(inserts.len(), 1);
        let HostCall::StorageCommit { writes, .. } = &calls[3] else {
            panic!("commit required");
        };
        assert_eq!(
            writes[0].expected_version.as_deref(),
            Some("9007199254740993")
        );
        assert_eq!(
            writes[0].expiry,
            super::super::protocol::HostKvExpiry::Preserve
        );
    }
}

#[test]
fn engines_storage_validation_rejects_invalid_batches_before_host_call() {
    for (name, engine) in engines() {
        for expression in [
            "$this.kv.getEntries('a')",
            "$this.kv.getEntries(Array(129).fill('a'))",
            "$this.kv.getEntries([''])",
            "$this.kv.getEntries([1])",
            "$this.kv.getEntries(['a'.repeat(256)])",
            "$this.kv.getEntries(['\\0'])",
            "$this.commit({writes:[],inserts:[]})",
            "$this.commit({writes:[],inserts:Array(33).fill({collection:'a',value:{}})})",
            "$this.commit({writes:Array(17).fill({key:'a',expectedVersion:null,value:1})})",
            "$this.commit({writes:[{key:'a',value:1}]})",
            "$this.commit({writes:[{key:'a',expectedVersion:'0',value:1}]})",
            "$this.commit({writes:[{key:'a',expectedVersion:'9223372036854775808',value:1}]})",
            "$this.commit({writes:[{key:'a',expectedVersion:null}]})",
            "$this.commit({writes:[{key:'a',expectedVersion:null,value:1},{key:'a',expectedVersion:null,value:2}]})",
            "$this.commit({writes:[{key:'a',expectedVersion:null,value:1,options:{ttl:0}}]})",
            "$this.commit({writes:[{key:'a',expectedVersion:null,value:1}],inserts:[{collection:'../a',value:{}}]})",
            "$this.commit({writes:[{key:'a',expectedVersion:null,value:1}],inserts:[{collection:'a',value:[]} ]})",
        ] {
            let host = Arc::new(StorageRecordingHost::default());
            let source = format!("export function run() {{ return {expression}; }}");
            assert!(
                engine
                    .execute(
                        request(&source, Value::Null, ResultMode::JsonRequired),
                        host.clone()
                    )
                    .is_err(),
                "{name}: {expression}"
            );
            assert!(
                host.calls.lock().unwrap().is_empty(),
                "{name}: {expression}"
            );
        }
    }
}
