use actix_web::{HttpResponse, Result, web};
use serde::Deserialize;

use crate::{DbPool, db};

#[derive(Deserialize)]
struct PathInfo {
    game_id: i32,
}

async fn get_info(info: web::Path<PathInfo>, db_pool: web::Data<DbPool>) -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().finish())
}

async fn get_anmts(info: web::Path<PathInfo>, db_pool: web::Data<DbPool>) -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().json(db::anmt::list_all(&db_pool, true, Some(info.game_id)).await?))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.route("/{game_id}/announcements", web::get().to(get_anmts));
}
