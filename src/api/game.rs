use actix_web::{
    HttpMessage, HttpResponse, Result,
    body::MessageBody,
    dev::{ServiceRequest, ServiceResponse},
    middleware::{self, Next},
    web,
};
use serde::Deserialize;

use crate::{
    DbPool,
    api::{error_handler, team},
    db,
    error::RbError,
    middleware::privilege::PrivilegeMiddleware,
    model::user::RbUserRole,
};

#[derive(Deserialize)]
struct PathInfo {
    game_id: i32,
}

async fn get_info(info: web::Path<PathInfo>, db_pool: web::Data<DbPool>) -> Result<HttpResponse> {
    let result = db::game::get_by_id(&db_pool, info.game_id).await?;
    if result.is_none() {
        RbError::not_found().err()?
    }

    Ok(HttpResponse::Ok().json(result))
}

async fn list_online(db_pool: web::Data<DbPool>) -> Result<HttpResponse> {
    let result = db::game::list_all(&db_pool, true, true).await?;

    Ok(HttpResponse::Ok().json(result))
}

async fn get_anmts(info: web::Path<PathInfo>, db_pool: web::Data<DbPool>) -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().json(db::anmt::list_all(&db_pool, true, Some(info.game_id)).await?))
}

// as games' visibilities don't change a lot, we ignore TOCTOU issues here
async fn check_game_id_middleware(
    req: ServiceRequest,
    next: Next<impl MessageBody>,
) -> Result<ServiceResponse<impl MessageBody>, actix_web::Error> {
    let game_id: i32 = req
        .match_info()
        .get("game_id")
        .and_then(|s| s.parse().ok())
        .ok_or_else(RbError::not_found)?;

    let user_role = *req.extensions().get().unwrap_or(&RbUserRole::Banned);

    let db_pool = req.app_data::<web::Data<DbPool>>().unwrap();

    if !db::game::exists(db_pool, game_id, user_role).await? {
        RbError::not_found().err()?
    }

    next.call(req).await
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.route("/online", web::get().to(list_online));
    cfg.service(
        web::scope("/{game_id}")
            .wrap(middleware::from_fn(check_game_id_middleware))
            .route("", web::get().to(get_info))
            .route("/announcements", web::get().to(get_anmts))
            .service(
                web::scope("")
                    .wrap(PrivilegeMiddleware::new(RbUserRole::User))
                    .service(
                        web::scope("/teams")
                            .configure(team::games_config)
                            .default_service(web::route().to(error_handler)),
                    )
                    .default_service(web::route().to(error_handler)),
            )
            .default_service(web::route().to(error_handler)),
    );
}
