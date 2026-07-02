use actix_web::{HttpResponse, Result, web};
use num_enum::IntoPrimitive;
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_repr::Serialize_repr;

use crate::{
    AppState, db, error::RbError, extractor::auth::AuthUser,
    middleware::privilege::PrivilegeMiddleware, model::user::RbUserRole,
};

#[derive(Deserialize)]
struct SystemSettingsRequest {
    registration_open: bool,
    require_email_verification: bool,
    max_sessions: i16,
    maintenance_enabled: bool,
    maintenance_message: String,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum SystemSettingsResult {
    Invalid = -1,
    Ok = 0,
}

#[derive(Serialize)]
struct SystemSettingsResponse {
    code: SystemSettingsResult,
    settings: db::system_settings::SystemSettings,
    email_delivery_enabled: bool,
}

fn valid_request(body: &SystemSettingsRequest, email_delivery_enabled: bool) -> bool {
    (1..=20).contains(&body.max_sessions)
        && body.maintenance_message.chars().count() <= 500
        && (!body.maintenance_enabled || !body.maintenance_message.is_empty())
        && (!body.require_email_verification || email_delivery_enabled)
}

async fn get(app: web::Data<AppState>) -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().json(SystemSettingsResponse {
        code: SystemSettingsResult::Ok,
        settings: app.system_settings.read().await.clone(),
        email_delivery_enabled: app.email.is_some(),
    }))
}

async fn update(
    body: web::Json<SystemSettingsRequest>,
    actor: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let mut body = body.into_inner();
    body.maintenance_message = body.maintenance_message.trim().to_string();
    if !valid_request(&body, app.email.is_some()) {
        return RbError::bad_req(SystemSettingsResult::Invalid.into()).http_err();
    }

    let previous = app.system_settings.read().await.clone();
    let settings = db::system_settings::update(
        &app.db,
        db::system_settings::SystemSettingsUpdate {
            registration_open: body.registration_open,
            require_email_verification: body.require_email_verification,
            max_sessions: body.max_sessions,
            maintenance_enabled: body.maintenance_enabled,
            maintenance_message: &body.maintenance_message,
            updated_by: actor.uid,
        },
    )
    .await?;
    *app.system_settings.write().await = settings.clone();

    db::event_log::insert_pool(
        &app.db,
        db::event_log::EventLogInput {
            event_type: "admin.system_settings_updated",
            event_scope: i16::from(db::event_log::EventScope::System),
            severity: i16::from(db::event_log::EventSeverity::Warning),
            user_id: Some(actor.uid),
            data: json!({
                "fields": {
                    "registration_open": previous.registration_open != settings.registration_open,
                    "require_email_verification": previous.require_email_verification != settings.require_email_verification,
                    "max_sessions": previous.max_sessions != settings.max_sessions,
                    "maintenance_enabled": previous.maintenance_enabled != settings.maintenance_enabled,
                    "maintenance_message": previous.maintenance_message != settings.maintenance_message,
                }
            }),
            ..Default::default()
        },
    )
    .await?;

    Ok(HttpResponse::Ok().json(SystemSettingsResponse {
        code: SystemSettingsResult::Ok,
        settings,
        email_delivery_enabled: app.email.is_some(),
    }))
}

#[cfg(test)]
mod tests {
    use super::{SystemSettingsRequest, valid_request};

    fn request() -> SystemSettingsRequest {
        SystemSettingsRequest {
            registration_open: true,
            require_email_verification: false,
            max_sessions: 3,
            maintenance_enabled: false,
            maintenance_message: String::new(),
        }
    }

    #[test]
    fn validates_system_settings_boundaries() {
        let mut body = request();
        assert!(valid_request(&body, false));
        body.max_sessions = 0;
        assert!(!valid_request(&body, false));
        body.max_sessions = 21;
        assert!(!valid_request(&body, false));
        body.max_sessions = 20;
        body.maintenance_enabled = true;
        assert!(!valid_request(&body, false));
        body.maintenance_message = "maintenance".to_string();
        assert!(valid_request(&body, false));
    }

    #[test]
    fn email_verification_requires_delivery_service() {
        let mut body = request();
        body.require_email_verification = true;
        assert!(!valid_request(&body, false));
        assert!(valid_request(&body, true));
    }
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/system-settings")
            .wrap(PrivilegeMiddleware::new(RbUserRole::Root))
            .route("", web::get().to(get))
            .route("", web::patch().to(update)),
    );
}
