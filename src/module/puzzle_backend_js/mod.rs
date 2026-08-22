use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use sqlx::PgConnection;
use tokio::runtime::Handle;

use crate::{
    AppState,
    db::puzzle_backend::{PuzzleBackend, PuzzleBackendCallLogInput},
    error::RbInternalError,
    game::judge::JudgeBackendOutput,
    module::sync::PuzzleBackendEventSync,
};

use self::{
    engine::{EngineRequest, ExecutionKind, ResultMode, active_engine, internal_err},
    host::{HostDispatcher, block_on_db, set_judge_conn, set_tokio_handle},
    protocol::{HOST_PROTOCOL_VERSION, HostValue},
};

mod engine;
mod host;
mod protocol;

const DEFAULT_BACKEND_FUNCTION_TIMEOUT: Duration = Duration::from_secs(5);
const JUDGE_EXECUTION_TIMEOUT: Duration = Duration::from_millis(500);

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

pub struct BackendExecution<T> {
    pub value: T,
    pub events: Vec<PuzzleBackendEventSync>,
}

struct EngineReport<T> {
    result: Result<T, RbInternalError>,
    events: Vec<PuzzleBackendEventSync>,
    console: Value,
    console_truncated: bool,
}

fn finish_execution<T>(
    app: &AppState,
    function_name: &str,
    runtime: &RuntimeContext,
    report: EngineReport<T>,
) -> Result<BackendExecution<T>, RbInternalError> {
    let duration_ms = i64::try_from(runtime.started_at.elapsed().as_millis()).unwrap_or(i64::MAX);
    let (execution_type, request_method) = match runtime.method.as_str() {
        "JUDGE" => ("judge", None),
        "HINT_PURCHASE" => ("hint_purchase", None),
        method => ("api", Some(method)),
    };
    let error = report.result.as_ref().err().map(ToString::to_string);
    if let Err(log_error) = block_on_db(crate::db::puzzle_backend::log_call(
        &app.db,
        PuzzleBackendCallLogInput {
            puzzle_id: runtime.puzzle_id,
            team_id: Some(runtime.team_id),
            user_id: runtime.user_id,
            execution_type,
            request_method,
            function_name,
            ok: report.result.is_ok(),
            duration_ms,
            submission_id: runtime.submission_id,
            hint_id: runtime.hint_id,
            error: error.as_deref(),
            console: &report.console,
            console_truncated: report.console_truncated,
        },
    )) {
        log::warn!(
            "failed to write puzzle backend execution log for puzzle {} function {}: {log_error}",
            runtime.puzzle_id,
            function_name
        );
    }

    report.result.map(|value| BackendExecution {
        value,
        events: report.events,
    })
}

fn bootstrap_metadata(runtime: &RuntimeContext) -> Value {
    json!({
        "protocolVersion": HOST_PROTOCOL_VERSION,
        "gameId": runtime.game_id,
        "team": {
            "id": runtime.team_id,
            "name": runtime.team_name,
        },
        "puzzle": {
            "id": runtime.puzzle_id,
            "title": runtime.puzzle_title,
        },
    })
}

fn api_argument(runtime: &RuntimeContext) -> Value {
    json!({
        "request": {
            "method": runtime.method,
            "query": runtime.query,
            "body": runtime.body,
        },
        "puzzle": {
            "id": runtime.puzzle_id,
            "gameId": runtime.game_id,
            "title": runtime.puzzle_title,
        },
        "user": {
            "id": runtime.user_id,
            "nickname": runtime.user_nickname,
        },
        "team": {
            "id": runtime.team_id,
            "name": runtime.team_name,
        },
        "apiName": runtime.api_name,
    })
}

fn judge_argument(runtime: &JudgeRuntimeContext) -> Value {
    json!({
        "request": {
            "userAnswer": runtime.user_answer,
            "normAnswer": runtime.norm_answer,
        },
        "puzzle": {
            "id": runtime.puzzle_id,
            "gameId": runtime.game_id,
            "title": runtime.puzzle_title,
        },
        "team": {
            "id": runtime.team_id,
            "name": runtime.team_name,
        },
        "user": {
            "id": runtime.user_id,
            "nickname": runtime.user_nickname,
        },
        "submission": {
            "id": runtime.submission.id,
            "createdAt": crate::serde_helpers::format_offset_datetime(&runtime.submission.ctime_at),
        },
        "apiName": "judge",
    })
}

fn hint_purchase_argument(runtime: &HintPurchaseRuntimeContext, function_name: &str) -> Value {
    json!({
        "puzzle": {
            "id": runtime.puzzle_id,
            "gameId": runtime.game_id,
            "title": runtime.puzzle_title,
        },
        "team": {
            "id": runtime.team_id,
            "name": runtime.team_name,
        },
        "user": {
            "id": runtime.user_id,
            "nickname": runtime.user_nickname,
        },
        "hint": {
            "id": runtime.hint_id,
            "title": runtime.hint_title,
            "costId": runtime.cost_id,
            "costAmount": runtime.cost_amount,
        },
        "purchase": {
            "currency": runtime.currency,
        },
        "apiName": function_name,
    })
}

fn execute_with_host<T>(
    app: AppState,
    runtime: RuntimeContext,
    request: EngineRequest,
    convert: impl FnOnce(HostValue) -> Result<T, RbInternalError>,
) -> EngineReport<T> {
    let function_name = request.function_name.clone();
    let dispatcher = Arc::new(HostDispatcher::new(app, runtime.clone()));
    let mut result = active_engine()
        .execute(request, dispatcher.clone())
        .and_then(convert);
    let capture = dispatcher.capture_report(result.is_ok(), &function_name);
    let events = match capture.events {
        Ok(events) => events,
        Err(error) => {
            if result.is_ok() {
                result = Err(internal_err(error.message));
            }
            vec![]
        }
    };
    EngineReport {
        result,
        events,
        console: capture.console,
        console_truncated: capture.console_truncated,
    }
}

fn engine_request(
    backend: PuzzleBackend,
    function_name: String,
    runtime: &RuntimeContext,
    execution_kind: ExecutionKind,
    argument: Value,
    result_mode: ResultMode,
) -> EngineRequest {
    EngineRequest {
        source: backend.source,
        function_name,
        execution_kind,
        argument,
        bootstrap_metadata: bootstrap_metadata(runtime),
        result_mode,
        wall_time_limit: runtime.timeout.saturating_sub(runtime.started_at.elapsed()),
    }
}

pub async fn execute_api(
    app: &AppState,
    backend: PuzzleBackend,
    api_name: String,
    runtime: RuntimeContext,
) -> Result<BackendExecution<Value>, RbInternalError> {
    let app = app.clone();
    let handle = Handle::current();
    tokio::task::spawn_blocking(move || {
        let _handle_guard = set_tokio_handle(handle);
        let argument = api_argument(&runtime);
        let request = engine_request(
            backend,
            api_name.clone(),
            &runtime,
            ExecutionKind::Api,
            argument,
            ResultMode::JsonRequired,
        );
        let report =
            execute_with_host(app.clone(), runtime.clone(), request, |value| match value {
                HostValue::Json(value) => Ok(value),
                HostValue::Undefined => Err(internal_err("value is undefined")),
            });
        finish_execution(&app, &api_name, &runtime, report)
    })
    .await
    .map_err(|error| internal_err(error.to_string()))?
}

pub async fn execute_judge(
    app: &AppState,
    backend: PuzzleBackend,
    function_name: String,
    runtime: JudgeRuntimeContext,
) -> Result<BackendExecution<Option<JudgeBackendOutput>>, RbInternalError> {
    let app = app.clone();
    let handle = Handle::current();
    tokio::task::spawn_blocking(move || {
        let _handle_guard = set_tokio_handle(handle);
        let runtime_context = judge_base_context(&function_name, &runtime);
        let report = execute_judge_sync(
            app.clone(),
            backend,
            function_name.clone(),
            runtime_context.clone(),
            runtime,
        );
        finish_execution(&app, &function_name, &runtime_context, report)
    })
    .await
    .map_err(|error| internal_err(error.to_string()))?
}

fn execute_judge_sync(
    app: AppState,
    backend: PuzzleBackend,
    function_name: String,
    runtime_context: RuntimeContext,
    runtime: JudgeRuntimeContext,
) -> EngineReport<Option<JudgeBackendOutput>> {
    let argument = judge_argument(&runtime);
    let request = engine_request(
        backend,
        function_name,
        &runtime_context,
        ExecutionKind::Judge,
        argument,
        ResultMode::UndefinedAllowed,
    );
    execute_with_host(app, runtime_context, request, |value| match value {
        HostValue::Undefined => Ok(None),
        HostValue::Json(value) => {
            let output: JudgeBackendOutput =
                serde_json::from_value(value).map_err(|error| internal_err(error.to_string()))?;
            if output.ignored.is_some() && output.action.is_none() {
                return Err(internal_err("judge output ignored requires action"));
            }
            Ok(Some(output))
        }
    })
}

pub async fn execute_judge_conn(
    app: &AppState,
    conn: &mut PgConnection,
    backend: PuzzleBackend,
    function_name: String,
    runtime: JudgeRuntimeContext,
) -> Result<BackendExecution<Option<JudgeBackendOutput>>, RbInternalError> {
    sqlx::query!("SET LOCAL statement_timeout = '500ms';")
        .execute(&mut *conn)
        .await?;
    sqlx::query!("SET LOCAL lock_timeout = '500ms';")
        .execute(&mut *conn)
        .await?;

    let runtime_context = judge_base_context(&function_name, &runtime);
    let handle = Handle::current();
    let result = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let _conn_guard = set_judge_conn(conn);
                let _handle_guard = set_tokio_handle(handle);
                let report = execute_judge_sync(
                    app.clone(),
                    backend,
                    function_name.clone(),
                    runtime_context.clone(),
                    runtime,
                );
                finish_execution(app, &function_name, &runtime_context, report)
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
    backend: PuzzleBackend,
    function_name: String,
    runtime: HintPurchaseRuntimeContext,
) -> Result<BackendExecution<()>, RbInternalError> {
    let app = app.clone();
    let handle = Handle::current();
    tokio::task::spawn_blocking(move || {
        let _handle_guard = set_tokio_handle(handle);
        let runtime_context = hint_base_context(&function_name, &runtime);
        let request = engine_request(
            backend,
            function_name.clone(),
            &runtime_context,
            ExecutionKind::HintPurchase,
            hint_purchase_argument(&runtime, &function_name),
            ResultMode::Ignored,
        );
        let report = execute_with_host(app.clone(), runtime_context.clone(), request, |_| Ok(()));
        finish_execution(&app, &function_name, &runtime_context, report)
    })
    .await
    .map_err(|error| internal_err(error.to_string()))?
}

fn judge_base_context(function_name: &str, runtime: &JudgeRuntimeContext) -> RuntimeContext {
    RuntimeContext {
        game_id: runtime.game_id,
        method: "JUDGE".to_string(),
        puzzle_id: runtime.puzzle_id,
        team_id: runtime.team_id,
        user_id: runtime.user_id,
        api_name: function_name.to_string(),
        submission_id: Some(runtime.submission.id),
        hint_id: None,
        query: Value::Null,
        body: Value::Null,
        puzzle_title: runtime.puzzle_title.clone(),
        user_nickname: runtime.user_nickname.clone(),
        team_name: runtime.team_name.clone(),
        started_at: Instant::now(),
        timeout: JUDGE_EXECUTION_TIMEOUT,
    }
}

fn hint_base_context(function_name: &str, runtime: &HintPurchaseRuntimeContext) -> RuntimeContext {
    RuntimeContext {
        game_id: runtime.game_id,
        method: "HINT_PURCHASE".to_string(),
        puzzle_id: runtime.puzzle_id,
        team_id: runtime.team_id,
        user_id: runtime.user_id,
        api_name: function_name.to_string(),
        submission_id: None,
        hint_id: Some(runtime.hint_id),
        query: Value::Null,
        body: Value::Null,
        puzzle_title: runtime.puzzle_title.clone(),
        user_nickname: runtime.user_nickname.clone(),
        team_name: runtime.team_name.clone(),
        started_at: Instant::now(),
        timeout: DEFAULT_BACKEND_FUNCTION_TIMEOUT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let value = judge_argument(&runtime);
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
}
