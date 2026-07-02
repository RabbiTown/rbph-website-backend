mod admin;
mod auth;
mod game;
mod notification;
mod puzzle;
mod puzzle_backend;
mod round;
mod sync;
mod system;
mod team;
mod ticket;
mod user;

use actix_web::{
    HttpResponse, Result,
    error::{self},
    web,
};

use crate::{middleware::privilege::PrivilegeMiddleware, model::user::RbUserRole};

async fn error_handler() -> Result<HttpResponse> {
    Err(error::ErrorForbidden("rbph-website"))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.configure(system::config)
        .service(
            web::scope("/auth")
                .configure(auth::config)
                .default_service(web::route().to(error_handler)),
        )
        .service(
            web::scope("/user")
                .wrap(PrivilegeMiddleware::new(RbUserRole::User))
                .configure(user::config)
                .default_service(web::route().to(error_handler)),
        )
        .service(
            web::scope("/games")
                .configure(game::config)
                .default_service(web::route().to(error_handler)),
        )
        .service(
            web::scope("/teams")
                .wrap(PrivilegeMiddleware::new(RbUserRole::User))
                .configure(team::teams_config)
                .default_service(web::route().to(error_handler)),
        )
        .service(
            web::scope("/puzzles")
                .wrap(PrivilegeMiddleware::new(RbUserRole::User))
                .configure(puzzle::puzzles_config)
                .default_service(web::route().to(error_handler)),
        )
        .service(
            web::scope("/hints")
                .wrap(PrivilegeMiddleware::new(RbUserRole::User))
                .configure(puzzle::hints_config)
                .default_service(web::route().to(error_handler)),
        )
        .service(
            web::scope("/rounds")
                .wrap(PrivilegeMiddleware::new(RbUserRole::User))
                .configure(round::rounds_config)
                .default_service(web::route().to(error_handler)),
        )
        .service(
            web::scope("/tickets")
                .wrap(PrivilegeMiddleware::new(RbUserRole::User))
                .configure(ticket::tickets_config)
                .default_service(web::route().to(error_handler)),
        )
        .service(
            web::scope("/sync")
                .wrap(PrivilegeMiddleware::new(RbUserRole::User))
                .configure(sync::config)
                .default_service(web::route().to(error_handler)),
        )
        .service(
            web::scope("/admin")
                .wrap(PrivilegeMiddleware::new(RbUserRole::Admin))
                .configure(admin::config)
                .default_service(web::route().to(error_handler)),
        )
        .default_service(web::route().to(error_handler));
}
