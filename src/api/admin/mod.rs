mod announcement;
mod asset;
mod content;
mod feature;
mod game;
mod hint;
mod log;
mod puzzle;
mod puzzle_backend;
mod release;
mod round;
mod system_settings;
mod team;
mod user;

use actix_web::web;

use crate::api::error_handler;

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("games")
            .configure(feature::config)
            .configure(game::config)
            .configure(release::config)
            .configure(team::config)
            .default_service(web::route().to(error_handler)),
    )
    .configure(asset::config)
    .configure(content::config)
    .configure(hint::config)
    .configure(log::config)
    .configure(puzzle_backend::config)
    .configure(puzzle::config)
    .configure(round::config);
    cfg.configure(announcement::config)
        .configure(user::config)
        .configure(system_settings::config);
}
