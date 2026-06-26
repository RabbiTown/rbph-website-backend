use actix_web::{HttpResponse, Result, web};
use num_enum::IntoPrimitive;
use serde::{Deserialize, Serialize};
use serde_repr::Serialize_repr;

use crate::{AppState, db};

#[derive(Deserialize)]
struct LogListQuery {
    scope: Option<i16>,
    severity: Option<i16>,
    event_type: Option<String>,
    game_id: Option<i32>,
    team_id: Option<i32>,
    user_id: Option<i32>,
    page: Option<i64>,
    limit: Option<i64>,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum LogAdminResult {
    Ok = 0,
}

#[derive(Serialize)]
struct LogAdminListResponse {
    code: LogAdminResult,
    logs: Vec<db::event_log::EventLogData>,
    total: i64,
}

async fn list(query: web::Query<LogListQuery>, app: web::Data<AppState>) -> Result<HttpResponse> {
    let event_type = query
        .event_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let page = query.page.unwrap_or(1).max(1);
    let offset = (page - 1) * limit;
    let logs = db::event_log::list_admin_logs(
        &app.db,
        db::event_log::AdminLogQuery {
            scope: query.scope,
            severity: query.severity,
            event_type,
            game_id: query.game_id,
            team_id: query.team_id,
            user_id: query.user_id,
            offset,
            limit,
        },
    )
    .await?;
    let total = db::event_log::count_admin_logs(
        &app.db,
        query.scope,
        query.severity,
        event_type,
        query.game_id,
        query.team_id,
        query.user_id,
    )
    .await?;

    Ok(HttpResponse::Ok().json(LogAdminListResponse {
        code: LogAdminResult::Ok,
        logs,
        total,
    }))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.route("/logs", web::get().to(list));
}
