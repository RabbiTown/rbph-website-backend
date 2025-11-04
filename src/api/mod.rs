mod auth;
mod user;

use actix_web::{
    HttpResponse, Result,
    error::{self},
    web,
};

use crate::{middleware::privilege::PrivilegeMiddleware, model::user::RbUserRole};

async fn error_handler() -> Result<HttpResponse> {
    Err(error::ErrorForbidden("forbidden"))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("auth")
            .configure(auth::config)
            .default_service(web::route().to(error_handler)),
    )
    .service(
        web::scope("user")
            .wrap(PrivilegeMiddleware::new(RbUserRole::User))
            .configure(user::config)
            .default_service(web::route().to(error_handler)),
    )
    .default_service(web::route().to(error_handler));
}
