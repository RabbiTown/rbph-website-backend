use actix_web::{HttpResponse, Result, web};
use serde::Serialize;

use crate::AppState;

#[derive(Serialize)]
struct SystemStatusResponse {
    code: i32,
    registration_open: bool,
    require_email_verification: bool,
    leaderboard_refresh_interval_seconds: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    footer_additional_info: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    no_game_message: Option<String>,
    maintenance_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    maintenance_message: Option<String>,
}

async fn status(app: web::Data<AppState>) -> Result<HttpResponse> {
    let settings = app.system_settings.read().await;
    Ok(HttpResponse::Ok().json(SystemStatusResponse {
        code: 0,
        registration_open: settings.registration_open,
        require_email_verification: settings.require_email_verification,
        leaderboard_refresh_interval_seconds: settings.leaderboard_refresh_interval_seconds,
        footer_additional_info: (!settings.footer_additional_info.is_empty())
            .then(|| settings.footer_additional_info.clone()),
        no_game_message: (!settings.no_game_message.is_empty())
            .then(|| settings.no_game_message.clone()),
        maintenance_enabled: settings.maintenance_enabled,
        maintenance_message: settings
            .maintenance_enabled
            .then(|| settings.maintenance_message.clone()),
    }))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.route("/system/status", web::get().to(status));
}
