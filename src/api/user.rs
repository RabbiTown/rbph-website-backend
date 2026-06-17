use actix_web::{HttpResponse, Result, web};
use serde::Deserialize;
use serde_json::json;

use crate::{AppState, db, error::RbError, extractor::auth::AuthUser};

pub async fn hello() -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().body("wowwo so privleged"))
}

pub async fn info(user: AuthUser, app: web::Data<AppState>) -> Result<HttpResponse> {
    let result = db::user::get_display_by_id(&app.db, user.uid).await?;

    Ok(HttpResponse::Ok().json(result))
}

#[derive(Deserialize)]
pub struct UserProfileUpdateRequest {
    nickname: String,
    bio: Option<String>,
}

pub async fn update_info(
    user: AuthUser,
    req: web::Json<UserProfileUpdateRequest>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let nickname = req.nickname.trim();
    let bio = req
        .bio
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if nickname.is_empty()
        || nickname.chars().count() > 60
        || bio.is_some_and(|value| value.chars().count() > 200)
    {
        return RbError::bad_req(-2).http_err();
    }

    let result = db::user::update_profile(&app.db, user.uid, nickname, bio).await?;
    db::event_log::insert_pool(
        &app.db,
        db::event_log::EventLogInput {
            event_type: "user.profile_updated",
            event_scope: i16::from(db::event_log::EventScope::System),
            severity: i16::from(db::event_log::EventSeverity::Info),
            user_id: Some(user.uid),
            data: json!({
                "fields": {
                    "nickname": true,
                    "bio": req.bio.is_some()
                }
            }),
            ..Default::default()
        },
    )
    .await?;

    Ok(HttpResponse::Ok().json(result))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.route("/hello", web::get().to(hello))
        .route("/info", web::get().to(info))
        .route("/info", web::patch().to(update_info));
}
