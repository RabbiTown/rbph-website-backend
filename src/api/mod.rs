mod auth;

use std::fmt::Debug;

use actix_web::{
    HttpResponse, ResponseError, Result,
    error::{self},
    web,
};
use derive_more::Display;
use rand::Rng;
use serde::Serialize;

async fn error_handler() -> Result<HttpResponse> {
    Err(error::ErrorForbidden("forbidden"))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("auth")
            .configure(auth::config)
            .default_service(web::route().to(error_handler)),
    )
    .default_service(web::route().to(error_handler));
}
