use std::fmt::Debug;

use actix_session::SessionInsertError;
use actix_web::{HttpResponse, ResponseError, error, http::StatusCode};
use deadpool_redis::redis::RedisError;
use derive_more::Display;
use rand::Rng;
use serde::Serialize;

#[derive(Debug, Serialize, Display)]
#[display("code = {code} ; {message:?}")]
pub struct RbError {
    pub code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,

    #[serde(skip_serializing)]
    pub status_code: StatusCode,
}

impl RbError {
    pub fn new(code: i32, message: &str) -> Self {
        Self {
            code: code,
            message: Some(message.to_string()),
            status_code: StatusCode::BAD_REQUEST,
        }
    }

    pub fn bad_req(code: i32) -> Self {
        Self {
            code: code,
            message: None,
            status_code: StatusCode::BAD_REQUEST,
        }
    }

    pub fn internal<T: Debug>(e: T) -> Self {
        let code: String = rand::rng()
            .sample_iter(rand::distr::Alphanumeric)
            .take(8)
            .map(char::from)
            .collect();

        log::warn!("internal server error ({}): {:?}", code, e);
        Self {
            code: -100,
            message: Some(format!("internal server error ({})", code)),
            status_code: StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn resp(&self) -> HttpResponse {
        HttpResponse::build(self.status_code).json(self)
    }

    pub fn err(self) -> Result<(), Self> {
        Err(self)
    }
}

impl ResponseError for RbError {
    fn error_response(&self) -> HttpResponse {
        HttpResponse::BadRequest().json(self)
    }
}

#[derive(Debug, Display)]
pub enum RbInternalError {
    Sql(sqlx::Error),
    Bcrypt(bcrypt::BcryptError),
    Redis(RedisError),
    RedisPool(deadpool::managed::PoolError<RedisError>),
    Json(serde_json::Error),
    Session(SessionInsertError),
}

impl ResponseError for RbInternalError {
    fn error_response(&self) -> actix_web::HttpResponse<actix_web::body::BoxBody> {
        match self {
            RbInternalError::Sql(err) => RbError::internal(err).resp(),
            RbInternalError::Bcrypt(err) => RbError::internal(err).resp(),
            RbInternalError::RedisPool(err) => RbError::internal(err).resp(),
            RbInternalError::Redis(err) => RbError::internal(err).resp(),
            RbInternalError::Json(err) => RbError::internal(err).resp(),
            RbInternalError::Session(err) => RbError::internal(err).resp(),
        }
    }
}

impl From<sqlx::Error> for RbInternalError {
    fn from(value: sqlx::Error) -> Self {
        RbInternalError::Sql(value)
    }
}

impl From<bcrypt::BcryptError> for RbInternalError {
    fn from(value: bcrypt::BcryptError) -> Self {
        RbInternalError::Bcrypt(value)
    }
}

impl From<deadpool::managed::PoolError<RedisError>> for RbInternalError {
    fn from(value: deadpool::managed::PoolError<RedisError>) -> Self {
        RbInternalError::RedisPool(value)
    }
}

impl From<RedisError> for RbInternalError {
    fn from(value: RedisError) -> Self {
        RbInternalError::Redis(value)
    }
}

impl From<serde_json::Error> for RbInternalError {
    fn from(value: serde_json::Error) -> Self {
        RbInternalError::Json(value)
    }
}

impl From<SessionInsertError> for RbInternalError {
    fn from(value: SessionInsertError) -> Self {
        RbInternalError::Session(value)
    }
}
