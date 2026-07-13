use actix_web::{HttpResponse, Result, web};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;

use crate::{AppState, db, error::RbError, extractor::auth::AuthUser, module::puzzle_backend_js};

#[derive(Deserialize)]
pub struct BackendPathInfo {
    pub puzzle_id: i32,
    pub api_name: String,
}

#[derive(Serialize)]
struct BackendResponse {
    code: i32,
    data: Value,
}

fn valid_backend_name(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        && value.len() <= 64
}

async fn call(
    req: actix_web::HttpRequest,
    path: web::Path<BackendPathInfo>,
    query: web::Query<HashMap<String, String>>,
    body: Option<web::Json<Value>>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !valid_backend_name(&path.api_name) {
        return RbError::not_found().http_err();
    }

    let team_id = user.req_team_id()?.ok_or(RbError::forbid())?;
    if !db::puzzle::can_team_access_puzzle(&app.db, team_id, path.puzzle_id).await? {
        return RbError::not_found().http_err();
    }
    let Some(backend) = db::puzzle_backend::get_backend(&app.db, path.puzzle_id).await? else {
        return RbError::not_found().http_err();
    };
    let Some(puzzle) = db::puzzle::admin_get(&app.db, path.puzzle_id).await? else {
        return RbError::not_found().http_err();
    };
    let user_info = db::user::get_display_by_id(&app.db, user.uid).await?;
    let Some(team) = db::team::get_by_id_show(&app.db, team_id).await? else {
        return RbError::not_found().http_err();
    };

    if !backend.enabled {
        return RbError::not_found().http_err();
    }
    if !backend.callable_function(&path.api_name) {
        return RbError::not_found().http_err();
    }

    let query_value = query
        .into_inner()
        .into_iter()
        .map(|(key, value)| (key, json!(value)))
        .collect::<serde_json::Map<_, _>>();

    let body = body.map(|body| body.into_inner()).unwrap_or(Value::Null);
    let result = puzzle_backend_js::execute_api(
        &app,
        backend,
        path.api_name.clone(),
        puzzle_backend_js::RuntimeContext {
            game_id: puzzle.game_id,
            method: req.method().as_str().to_string(),
            puzzle_id: path.puzzle_id,
            team_id,
            user_id: user.uid,
            api_name: path.api_name.clone(),
            submission_id: None,
            hint_id: None,
            query: Value::Object(query_value),
            body,
            puzzle_title: puzzle.title,
            user_nickname: user_info.nickname,
            team_name: team.name,
            started_at: std::time::Instant::now(),
            timeout: std::time::Duration::from_secs(5),
        },
    )
    .await;

    let data = match result {
        Ok(data) => data,
        Err(crate::error::RbInternalError::Other(message))
            if message == "export is not a function" =>
        {
            return RbError::not_found().http_err();
        }
        Err(err) => return Err(err.into()),
    };
    Ok(HttpResponse::Ok().json(BackendResponse { code: 0, data }))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.route("/backend/{api_name}", web::get().to(call))
        .route("/backend/{api_name}", web::post().to(call));
}
