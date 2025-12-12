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

use crate::{DbPool, KvPool, api::error_handler, db, error::RbError, extractor::auth::AuthUser};

#[derive(Deserialize)]
struct RoundPathInfo {
    round_id: i32,
}

async fn get_round(
    info: web::Path<RoundPathInfo>,
    user: AuthUser,
    db_pool: web::Data<DbPool>,
    kv_pool: web::Data<KvPool>,
) -> Result<HttpResponse> {
    let result = db::round::get_info_for_team_str(
        &db_pool,
        &kv_pool,
        info.round_id,
        user.game.unwrap().team_id,
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

    let db_pool = req.app_data::<web::Data<DbPool>>().unwrap();
    let kv_pool = req.app_data::<web::Data<KvPool>>().unwrap();

    match db::round::get_round_user_info(db_pool, kv_pool, user_id, round_id).await? {
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
