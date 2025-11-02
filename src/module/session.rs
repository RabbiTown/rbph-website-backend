use actix_session::Session;
use actix_web::{ResponseError, Result};
use deadpool_redis::Pool;
use derive_more::Display;

use crate::api::RbError;

#[derive(Debug, Display)]
pub enum SessionError {
    RedisError(sqlx::Error),
}

impl ResponseError for SessionError {
    fn error_response(&self) -> actix_web::HttpResponse<actix_web::body::BoxBody> {
        match self {
            SessionError::RedisError(err) => RbError::report_internal(err),
        }
    }
}

pub async fn put_user_session(
    pool: &Pool,
    sess: &Session,
    user_id: i32,
    max_session: usize,
) -> Result<()> {
    Ok(())
}
