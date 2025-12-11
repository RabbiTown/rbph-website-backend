use actix_web::{middleware, web};

use crate::api::error_handler;

// /rounds/...
pub fn rounds_config(cfg: &mut web::ServiceConfig) {
    cfg.service(web::scope("/{round_id}").default_service(web::route().to(error_handler)));
}
