use actix_web::ResponseError;
use deadpool_redis::redis::RedisError;
use derive_more::Display;
use sqlx::{PgPool, postgres::PgPoolOptions};

use crate::api::RbError;

pub mod user;

#[derive(Debug, Display)]
pub enum DbError {
    SqlError(sqlx::Error),
    BcryptError(bcrypt::BcryptError),
    RedisError(RedisError),
    PoolRedisError(deadpool::managed::PoolError<RedisError>),
    SerdeJsonError(serde_json::Error),
}

impl ResponseError for DbError {
    fn error_response(&self) -> actix_web::HttpResponse<actix_web::body::BoxBody> {
        match self {
            DbError::SqlError(err) => RbError::report_internal(err),
            DbError::BcryptError(err) => RbError::report_internal(err),
            DbError::PoolRedisError(err) => RbError::report_internal(err),
            DbError::RedisError(err) => RbError::report_internal(err),
            DbError::SerdeJsonError(err) => RbError::report_internal(err),
        }
    }
}

impl From<sqlx::Error> for DbError {
    fn from(value: sqlx::Error) -> Self {
        DbError::SqlError(value)
    }
}

impl From<bcrypt::BcryptError> for DbError {
    fn from(value: bcrypt::BcryptError) -> Self {
        DbError::BcryptError(value)
    }
}

impl From<deadpool::managed::PoolError<RedisError>> for DbError {
    fn from(value: deadpool::managed::PoolError<RedisError>) -> Self {
        DbError::PoolRedisError(value)
    }
}

impl From<RedisError> for DbError {
    fn from(value: RedisError) -> Self {
        DbError::RedisError(value)
    }
}

impl From<serde_json::Error> for DbError {
    fn from(value: serde_json::Error) -> Self {
        DbError::SerdeJsonError(value)
    }
}

pub async fn create_pool(url: &str) -> Result<PgPool, sqlx::Error> {
    let pool = PgPoolOptions::new().max_connections(5).connect(url).await?;
    sqlx::migrate!();
    Ok(pool)
}
