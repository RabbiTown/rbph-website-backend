mod game;

use actix_web::web;

use crate::api::error_handler;

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("games")
            .configure(game::config)
            .default_service(web::route().to(error_handler)),
    );
}
