use std::fmt::Debug;

use actix_session::SessionInsertError;
use actix_web::{HttpResponse, ResponseError, http::StatusCode};
use deadpool_redis::redis::RedisError;
use derive_more::Display;
use num_enum::IntoPrimitive;
use rand::Rng;
use serde::Serialize;

#[repr(i32)]
#[derive(IntoPrimitive)]
enum RbErrorCode {
    CaptchaUnavailable = -109,
    CaptchaInvalid = -108,
    Maintenance = -107,
    PasswordChangeRequired = -106,
    NotFound = -104,
    Forbidden = -103,
    Unauthorized = -101,
    InternalServerError = -100,
}

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
    pub fn msg(mut self, msg: impl Into<String>) -> Self {
        self.message = Some(msg.into());
        self
    }

    pub fn code(mut self, code: i32) -> Self {
        self.code = code;
        self
    }

    pub fn bad_req(code: i32) -> Self {
        Self {
            code,
            message: None,
            status_code: StatusCode::BAD_REQUEST,
        }
    }

    pub fn conflict(code: i32) -> Self {
        Self {
            code,
            message: None,
            status_code: StatusCode::CONFLICT,
        }
    }

    pub fn unprocessable(code: i32) -> Self {
        Self {
            code,
            message: None,
            status_code: StatusCode::UNPROCESSABLE_ENTITY,
        }
    }

    pub fn internal<T: Debug>(e: T) -> Self {
        let code: String = rand::rng()
            .sample_iter(rand::distr::Alphanumeric)
            .take(8)
            .map(char::from)
            .collect();

        log::warn!("Internal Server Error ({}): {:?}", code, e);
        Self {
            code: RbErrorCode::InternalServerError.into(),
            message: Some(format!("Internal Server Error ({code})")),
            status_code: StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn unauth() -> Self {
        Self {
            code: RbErrorCode::Unauthorized.into(),
            message: Some("Unauthorized".to_string()),
            status_code: StatusCode::UNAUTHORIZED,
        }
    }

    pub fn forbid() -> Self {
        Self {
            code: RbErrorCode::Forbidden.into(),
            message: Some("Forbidden".to_string()),
            status_code: StatusCode::FORBIDDEN,
        }
    }

    pub fn password_change_required() -> Self {
        Self {
            code: RbErrorCode::PasswordChangeRequired.into(),
            message: Some("Password change required".to_string()),
            status_code: StatusCode::FORBIDDEN,
        }
    }

    pub fn maintenance(message: impl Into<String>) -> Self {
        Self {
            code: RbErrorCode::Maintenance.into(),
            message: Some(message.into()),
            status_code: StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    pub fn captcha_invalid() -> Self {
        Self {
            code: RbErrorCode::CaptchaInvalid.into(),
            message: Some("Captcha verification failed".to_string()),
            status_code: StatusCode::BAD_REQUEST,
        }
    }

    pub fn captcha_unavailable() -> Self {
        Self {
            code: RbErrorCode::CaptchaUnavailable.into(),
            message: Some("Captcha service unavailable".to_string()),
            status_code: StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    pub fn not_found() -> Self {
        Self {
            code: RbErrorCode::NotFound.into(),
            message: Some("Not Found".to_string()),
            status_code: StatusCode::NOT_FOUND,
        }
    }

    pub fn resp(&self) -> HttpResponse {
        HttpResponse::build(self.status_code).json(self)
    }

    pub fn err(self) -> Result<(), Self> {
        Err(self)
    }

    pub fn http_err(self) -> Result<HttpResponse, actix_web::Error> {
        Err(self.into())
    }
}

impl ResponseError for RbError {
    fn error_response(&self) -> HttpResponse {
        self.resp()
    }
}

#[derive(Debug, Display)]
pub enum RbInternalError {
    Sql(sqlx::Error),
    Bcrypt(bcrypt::BcryptError),
    Io(std::io::Error),
    Zip(zip::result::ZipError),
    Redis(RedisError),
    RedisPool(deadpool::managed::PoolError<RedisError>),
    Json(serde_json::Error),
    Session(SessionInsertError),
    Askama(askama::Error),
    Lettre(lettre::error::Error),
    LettreAddress(lettre::address::AddressError),
    Other(String),
}

impl ResponseError for RbInternalError {
    fn error_response(&self) -> actix_web::HttpResponse<actix_web::body::BoxBody> {
        match self {
            RbInternalError::Sql(err) => RbError::internal(err).resp(),
            RbInternalError::Bcrypt(err) => RbError::internal(err).resp(),
            RbInternalError::Io(err) => RbError::internal(err).resp(),
            RbInternalError::Zip(err) => RbError::internal(err).resp(),
            RbInternalError::RedisPool(err) => RbError::internal(err).resp(),
            RbInternalError::Redis(err) => RbError::internal(err).resp(),
            RbInternalError::Json(err) => RbError::internal(err).resp(),
            RbInternalError::Session(err) => RbError::internal(err).resp(),
            RbInternalError::Askama(err) => RbError::internal(err).resp(),
            RbInternalError::Lettre(err) => RbError::internal(err).resp(),
            RbInternalError::LettreAddress(err) => RbError::internal(err).resp(),
            RbInternalError::Other(err) => RbError::internal(err).resp(),
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

impl From<std::io::Error> for RbInternalError {
    fn from(value: std::io::Error) -> Self {
        RbInternalError::Io(value)
    }
}

impl From<zip::result::ZipError> for RbInternalError {
    fn from(value: zip::result::ZipError) -> Self {
        RbInternalError::Zip(value)
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

impl From<askama::Error> for RbInternalError {
    fn from(value: askama::Error) -> Self {
        RbInternalError::Askama(value)
    }
}

impl From<lettre::error::Error> for RbInternalError {
    fn from(value: lettre::error::Error) -> Self {
        RbInternalError::Lettre(value)
    }
}

impl From<lettre::address::AddressError> for RbInternalError {
    fn from(value: lettre::address::AddressError) -> Self {
        RbInternalError::LettreAddress(value)
    }
}

impl From<String> for RbInternalError {
    fn from(value: String) -> Self {
        RbInternalError::Other(value)
    }
}

impl From<&str> for RbInternalError {
    fn from(value: &str) -> Self {
        value.to_string().into()
    }
}
