use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

use serde_json::Value;
use v8::{self as rusty_v8, Local};

use crate::error::RbInternalError;
use crate::module::puzzle_backend_js::{
    host::HostBridge,
    protocol::{HostCall, HostRequest, HostValue},
};

use super::{EngineRequest, JsEngine, ResultMode, internal_err};

const BOOTSTRAP_SOURCE: &str = include_str!("../bootstrap.js");
const TIMEOUT_MESSAGE: &str = "backend function execution timed out";

static V8_INITIALIZED: OnceLock<()> = OnceLock::new();

struct HostSlot(Arc<dyn HostBridge>);

pub(super) struct V8Engine;

fn initialize_v8() {
    V8_INITIALIZED.get_or_init(|| {
        let platform = rusty_v8::new_unprotected_default_platform(0, false).make_shared();
        rusty_v8::V8::initialize_platform(platform);
        rusty_v8::V8::initialize();
    });
}

unsafe extern "C" fn deny_wasm(
    _context: Local<'_, rusty_v8::Context>,
    _source: Local<'_, rusty_v8::String>,
) -> bool {
    false
}

fn reject_module_import<'s>(
    context: Local<'s, rusty_v8::Context>,
    specifier: Local<'s, rusty_v8::String>,
    _import_attributes: Local<'s, rusty_v8::FixedArray>,
    _referrer: Local<'s, rusty_v8::Module>,
) -> Option<Local<'s, rusty_v8::Module>> {
    rusty_v8::callback_scope!(unsafe scope, context);
    rusty_v8::scope!(let scope, scope);
    let specifier = specifier.to_rust_string_lossy(scope);
    let message = rusty_v8::String::new(
        scope,
        &format!("module imports are not supported: {specifier}"),
    )
    .or_else(|| rusty_v8::String::new(scope, "module imports are not supported"))?;
    let exception = rusty_v8::Exception::type_error(scope, message);
    scope.throw_exception(exception);
    None
}

fn reject_dynamic_import<'s>(
    scope: &mut rusty_v8::PinScope<'s, '_>,
    _host_defined_options: Local<'s, rusty_v8::Data>,
    _resource_name: Local<'s, rusty_v8::Value>,
    specifier: Local<'s, rusty_v8::String>,
    _import_attributes: Local<'s, rusty_v8::FixedArray>,
) -> Option<Local<'s, rusty_v8::Promise>> {
    let specifier = specifier.to_rust_string_lossy(scope);
    let message = rusty_v8::String::new(
        scope,
        &format!("module imports are not supported: {specifier}"),
    )?;
    let exception = rusty_v8::Exception::type_error(scope, message);
    let resolver = rusty_v8::PromiseResolver::new(scope)?;
    resolver.reject(scope, exception)?;
    Some(resolver.get_promise(scope))
}

fn throw_type_error(scope: &mut rusty_v8::PinScope<'_, '_>, message: impl AsRef<str>) {
    if let Some(message) = rusty_v8::String::new(scope, message.as_ref())
        .or_else(|| rusty_v8::String::new(scope, "V8 host bridge error"))
    {
        let exception = rusty_v8::Exception::type_error(scope, message);
        scope.throw_exception(exception);
    }
}

fn json_to_v8<'s>(
    scope: &rusty_v8::PinScope<'s, '_>,
    value: &Value,
) -> Result<Local<'s, rusty_v8::Value>, String> {
    let json = serde_json::to_string(value).map_err(|error| error.to_string())?;
    let json = rusty_v8::String::new(scope, &json)
        .ok_or_else(|| "JSON value is too large for V8".to_string())?;
    rusty_v8::json::parse(scope, json).ok_or_else(|| "invalid JSON value".to_string())
}

fn v8_to_json(
    scope: &rusty_v8::PinScope<'_, '_>,
    value: Local<'_, rusty_v8::Value>,
) -> Result<Value, String> {
    let json =
        rusty_v8::json::stringify(scope, value).ok_or_else(|| "value is undefined".to_string())?;
    serde_json::from_str(&json.to_rust_string_lossy(scope)).map_err(|error| error.to_string())
}

fn host_callback(
    scope: &mut rusty_v8::PinScope<'_, '_>,
    args: rusty_v8::FunctionCallbackArguments,
    mut return_value: rusty_v8::ReturnValue<rusty_v8::Value>,
) {
    let result = (|| -> Result<Option<Local<'_, rusty_v8::Value>>, String> {
        let operation = args.get(0);
        if !operation.is_string() {
            return Err("host operation must be a string".to_string());
        }
        let operation = operation.to_rust_string_lossy(scope);
        let payload = args.get(1);
        let payload = v8_to_json(scope, payload)?;
        let mut payload = payload
            .as_object()
            .cloned()
            .ok_or_else(|| "host payload must be an object".to_string())?;
        payload.insert("operation".to_string(), Value::String(operation));
        let call = serde_json::from_value::<HostCall>(Value::Object(payload))
            .map_err(|error| error.to_string())?;
        let host = scope
            .get_slot::<HostSlot>()
            .ok_or_else(|| "V8 host bridge is unavailable".to_string())?
            .0
            .clone();
        match host
            .call(HostRequest::current(call))
            .map_err(|error| error.message)?
        {
            HostValue::Json(value) => json_to_v8(scope, &value).map(Some),
            HostValue::Undefined => Ok(None),
        }
    })();

    match result {
        Ok(Some(value)) => return_value.set(value),
        Ok(None) => {}
        Err(message) => throw_type_error(scope, message),
    }
}

macro_rules! caught_message {
    ($scope:expr, $fallback:expr) => {{
        let caught = $scope.stack_trace().or_else(|| $scope.exception());
        caught
            .map(|value| value.to_rust_string_lossy($scope))
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| $fallback.to_string())
    }};
}

fn resolve_promise<'s>(
    scope: &mut rusty_v8::PinScope<'s, '_>,
    value: Local<'s, rusty_v8::Value>,
    label: &str,
) -> Result<Local<'s, rusty_v8::Value>, RbInternalError> {
    if !value.is_promise() {
        return Ok(value);
    }
    let promise = Local::<rusty_v8::Promise>::try_from(value)
        .map_err(|_| internal_err("invalid V8 promise"))?;
    scope.perform_microtask_checkpoint();
    match promise.state() {
        rusty_v8::PromiseState::Fulfilled => Ok(promise.result(scope)),
        rusty_v8::PromiseState::Rejected => Err(internal_err(format!(
            "{label} rejected: {}",
            promise.result(scope).to_rust_string_lossy(scope)
        ))),
        rusty_v8::PromiseState::Pending => {
            Err(internal_err(format!("{label} promise is still pending")))
        }
    }
}

fn execute_in_isolate(
    isolate: &mut rusty_v8::OwnedIsolate,
    request: &EngineRequest,
    host: Arc<dyn HostBridge>,
) -> Result<HostValue, RbInternalError> {
    let _execution_kind = request.execution_kind;
    isolate.set_slot(HostSlot(host));
    isolate.set_allow_atomics_wait(false);
    isolate.set_allow_wasm_code_generation_callback(deny_wasm);
    isolate.set_host_import_module_dynamically_callback(reject_dynamic_import);
    isolate.set_microtasks_policy(rusty_v8::MicrotasksPolicy::Explicit);

    rusty_v8::scope!(let handle_scope, isolate);
    let context = rusty_v8::Context::new(handle_scope, Default::default());
    let context_scope = &mut rusty_v8::ContextScope::new(handle_scope, context);
    rusty_v8::tc_scope!(let scope, context_scope);

    let global = context.global(scope);
    let native_name = rusty_v8::String::new(scope, "__rbph_native_call")
        .ok_or_else(|| internal_err("failed to allocate V8 host bridge name"))?;
    let native = rusty_v8::Function::new(scope, host_callback)
        .ok_or_else(|| internal_err(caught_message!(scope, "failed to create V8 host bridge")))?;
    if global.set(scope, native_name.into(), native.into()) != Some(true) {
        return Err(internal_err(caught_message!(
            scope,
            "failed to install V8 host bridge"
        )));
    }

    let metadata_name = rusty_v8::String::new(scope, "__rbph_bootstrap_metadata")
        .ok_or_else(|| internal_err("failed to allocate V8 metadata name"))?;
    let metadata = json_to_v8(scope, &request.bootstrap_metadata).map_err(internal_err)?;
    if global.set(scope, metadata_name.into(), metadata) != Some(true) {
        return Err(internal_err(caught_message!(
            scope,
            "failed to install V8 bootstrap metadata"
        )));
    }

    let bootstrap_source = rusty_v8::String::new(scope, BOOTSTRAP_SOURCE)
        .ok_or_else(|| internal_err("V8 bootstrap source is too large"))?;
    let bootstrap = rusty_v8::Script::compile(scope, bootstrap_source, None)
        .ok_or_else(|| internal_err(caught_message!(scope, "failed to compile V8 bootstrap")))?;
    bootstrap
        .run(scope)
        .ok_or_else(|| internal_err(caught_message!(scope, "failed to run V8 bootstrap")))?;

    let source = rusty_v8::String::new(scope, &request.source)
        .ok_or_else(|| internal_err("backend module source is too large for V8"))?;
    let resource_name = rusty_v8::String::new(scope, "puzzle-backend.js")
        .ok_or_else(|| internal_err("failed to allocate V8 module resource name"))?;
    let origin = rusty_v8::ScriptOrigin::new(
        scope,
        resource_name.into(),
        0,
        0,
        false,
        -1,
        None,
        false,
        false,
        true,
        None,
    );
    let mut source = rusty_v8::script_compiler::Source::new(source, Some(&origin));
    let module = rusty_v8::script_compiler::compile_module(scope, &mut source)
        .ok_or_else(|| internal_err(caught_message!(scope, "failed to compile backend module")))?;
    module
        .instantiate_module(scope, reject_module_import)
        .ok_or_else(|| {
            internal_err(caught_message!(
                scope,
                "failed to instantiate backend module"
            ))
        })?;
    let evaluation = module
        .evaluate(scope)
        .ok_or_else(|| internal_err(caught_message!(scope, "failed to evaluate backend module")))?;
    resolve_promise(scope, evaluation, "module")?;

    let namespace = module
        .get_module_namespace()
        .to_object(scope)
        .ok_or_else(|| internal_err("backend module namespace is unavailable"))?;
    let function_name = rusty_v8::String::new(scope, &request.function_name)
        .ok_or_else(|| internal_err("backend function name is too large for V8"))?;
    let function = namespace
        .get(scope, function_name.into())
        .ok_or_else(|| internal_err(caught_message!(scope, "failed to read module export")))?;
    let function = Local::<rusty_v8::Function>::try_from(function)
        .map_err(|_| internal_err("export is not a function"))?;
    let argument = json_to_v8(scope, &request.argument).map_err(internal_err)?;
    let receiver = rusty_v8::undefined(scope).into();
    let result = function
        .call(scope, receiver, &[argument])
        .ok_or_else(|| internal_err(caught_message!(scope, "backend function threw")))?;
    let result = resolve_promise(scope, result, "api")?;

    match request.result_mode {
        ResultMode::JsonRequired => Ok(HostValue::Json(
            v8_to_json(scope, result).map_err(internal_err)?,
        )),
        ResultMode::UndefinedAllowed if result.is_undefined() => Ok(HostValue::Undefined),
        ResultMode::UndefinedAllowed => Ok(HostValue::Json(
            v8_to_json(scope, result).map_err(internal_err)?,
        )),
        ResultMode::Ignored => Ok(HostValue::Undefined),
    }
}

impl JsEngine for V8Engine {
    fn execute(
        &self,
        request: EngineRequest,
        host: Arc<dyn HostBridge>,
    ) -> Result<HostValue, RbInternalError> {
        initialize_v8();

        let mut isolate = rusty_v8::Isolate::new(rusty_v8::CreateParams::default());
        let handle = isolate.thread_safe_handle();
        let timed_out = AtomicBool::new(false);
        let result = thread::scope(|scope| {
            let (done_tx, done_rx) = mpsc::sync_channel(1);
            let timed_out = &timed_out;
            let watchdog = scope.spawn(move || {
                if matches!(
                    done_rx.recv_timeout(request.wall_time_limit),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    timed_out.store(true, Ordering::Release);
                    handle.terminate_execution();
                }
            });

            let result = execute_in_isolate(&mut isolate, &request, host);
            let _ = done_tx.send(());
            watchdog.join().expect("V8 watchdog thread panicked");
            result
        });

        if timed_out.load(Ordering::Acquire) {
            isolate.cancel_terminate_execution();
            Err(internal_err(TIMEOUT_MESSAGE))
        } else {
            result
        }
    }
}
