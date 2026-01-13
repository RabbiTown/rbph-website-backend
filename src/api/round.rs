use actix_session::SessionExt;
use actix_web::{
    HttpMessage, HttpResponse, Result,
    body::MessageBody,
    dev::{ServiceRequest, ServiceResponse},
    http::header::ContentType,
    middleware::{self, Next},
    web,
};
use serde::Deserialize;

use crate::{AppState, api::error_handler, db, error::RbError, extractor::auth::AuthUser};

#[derive(Deserialize)]
struct RoundPathInfo {
    round_id: i32,
}

async fn get_round(
    info: web::Path<RoundPathInfo>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let result = db::round::get_info_for_team_str(
        &app.db,
        &app.kv,
        info.round_id,
        user.get_team_id().ok_or(RbError::forbid())?,
    )
    .await?;
    if result.is_none() {
        RbError::not_found().err()?
    }

    Ok(HttpResponse::Ok()
        .content_type(ContentType::json())
        .body(result.unwrap()))
}

async fn check_round_middleware(
    req: ServiceRequest,
    next: Next<impl MessageBody>,
) -> Result<ServiceResponse<impl MessageBody>, actix_web::Error> {
    let round_id: i32 = req
        .match_info()
        .get("round_id")
        .and_then(|s| s.parse().ok())
        .ok_or_else(RbError::not_found)?;

    let user_id: i32 = req
        .get_session()
        .get::<i32>("user_id")
        .ok()
        .flatten()
        .ok_or_else(RbError::not_found)?;

    let app = req.app_data::<web::Data<AppState>>().unwrap();

    match db::round::get_round_user_info(&app.db, user_id, round_id).await? {
        Some(info) => {
            req.extensions_mut().insert(info);
        }
        None => {
            RbError::not_found().err()?;
        }
    };

    next.call(req).await
}

// /rounds/...
pub fn rounds_config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/{round_id}")
            .wrap(middleware::from_fn(check_round_middleware))
            .route("", web::get().to(get_round))
            .default_service(web::route().to(error_handler)),
    );
}
