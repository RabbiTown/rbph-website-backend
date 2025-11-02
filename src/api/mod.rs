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

#[derive(Debug, Serialize, Display)]
#[display("code = {code} ; {message:?}")]
pub struct RbError {
    pub code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl RbError {
    pub fn with_code(code: i32) -> Self {
        Self {
            code: code,
            message: None,
        }
    }

    pub fn report_internal<T: Debug>(e: T) -> HttpResponse {
        let code: String = rand::rng()
            .sample_iter(rand::distr::Alphanumeric)
            .take(8)
            .map(char::from)
            .collect();

        log::warn!("internal server error ({}): {:?}", code, e);

        HttpResponse::InternalServerError().json(RbError {
            code: -100,
            message: Some(format!("internal server error ({})", code)),
        })
    }

    pub fn new(code: i32, message: &str) -> Self {
        Self {
            code: code,
            message: Some(message.to_string()),
        }
    }

    fn err(self) -> Result<(), Self> {
        Err(self)
    }

    fn intern_err<T>(&self, e: T) -> Result<(), actix_web::Error>
    where
        T: std::fmt::Debug + std::fmt::Display + 'static,
    {
        let resp = HttpResponse::InternalServerError().json(self);
        Err(error::InternalError::from_response(e, resp).into())
    }
}

impl ResponseError for RbError {
    fn error_response(&self) -> HttpResponse {
        HttpResponse::BadRequest().json(self)
    }
}

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
