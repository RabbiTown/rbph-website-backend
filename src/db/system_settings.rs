use serde::Serialize;
use time::OffsetDateTime;

use crate::{DbPool, error::RbInternalError};

#[derive(Clone, Serialize)]
pub struct SystemSettings {
    pub registration_open: bool,
    pub require_email_verification: bool,
    pub captcha_login_required: bool,
    pub captcha_registration_required: bool,
    pub max_sessions: i16,
    pub max_websocket_connections: i16,
    pub maintenance_enabled: bool,
    pub maintenance_message: String,
    pub updated_by: Option<i32>,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub updated_at: OffsetDateTime,
}

pub struct SystemSettingsUpdate<'a> {
    pub registration_open: bool,
    pub require_email_verification: bool,
    pub captcha_login_required: bool,
    pub captcha_registration_required: bool,
    pub max_sessions: i16,
    pub max_websocket_connections: i16,
    pub maintenance_enabled: bool,
    pub maintenance_message: &'a str,
    pub updated_by: i32,
}

pub async fn get(pool: &DbPool) -> Result<SystemSettings, RbInternalError> {
    Ok(sqlx::query_as!(
        SystemSettings,
        "SELECT registration_open, require_email_verification,
            captcha_login_required, captcha_registration_required, max_sessions,
            max_websocket_connections,
            maintenance_enabled, maintenance_message, updated_by, updated_at
        FROM rb_system_settings WHERE singleton = TRUE"
    )
    .fetch_one(pool)
    .await?)
}

pub async fn disable_unavailable_auth_features(
    pool: &DbPool,
    captcha_available: bool,
    email_available: bool,
) -> Result<SystemSettings, RbInternalError> {
    sqlx::query!(
        "UPDATE rb_system_settings SET
            require_email_verification = require_email_verification AND $1,
            captcha_login_required = captcha_login_required AND $2,
            captcha_registration_required = captcha_registration_required AND $2,
            updated_at = CURRENT_TIMESTAMP
        WHERE singleton = TRUE AND (
            (NOT $1 AND require_email_verification)
            OR (NOT $2 AND (captcha_login_required OR captcha_registration_required))
        )",
        email_available,
        captcha_available,
    )
    .execute(pool)
    .await?;

    get(pool).await
}

pub async fn update(
    pool: &DbPool,
    data: SystemSettingsUpdate<'_>,
) -> Result<SystemSettings, RbInternalError> {
    Ok(sqlx::query_as!(
        SystemSettings,
        "UPDATE rb_system_settings SET
            registration_open = $1,
            require_email_verification = $2,
            captcha_login_required = $3,
            captcha_registration_required = $4,
            max_sessions = $5,
            max_websocket_connections = $6,
            maintenance_enabled = $7,
            maintenance_message = $8,
            updated_by = $9,
            updated_at = CURRENT_TIMESTAMP
        WHERE singleton = TRUE
        RETURNING registration_open, require_email_verification,
            captcha_login_required, captcha_registration_required, max_sessions,
            max_websocket_connections,
            maintenance_enabled, maintenance_message, updated_by, updated_at",
        data.registration_open,
        data.require_email_verification,
        data.captcha_login_required,
        data.captcha_registration_required,
        data.max_sessions,
        data.max_websocket_connections,
        data.maintenance_enabled,
        data.maintenance_message,
        data.updated_by,
    )
    .fetch_one(pool)
    .await?)
}
