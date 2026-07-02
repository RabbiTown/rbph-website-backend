use serde::Serialize;
use time::OffsetDateTime;

use crate::{DbPool, error::RbInternalError};

#[derive(Clone, Serialize)]
pub struct SystemSettings {
    pub registration_open: bool,
    pub require_email_verification: bool,
    pub max_sessions: i16,
    pub maintenance_enabled: bool,
    pub maintenance_message: String,
    pub updated_by: Option<i32>,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub updated_at: OffsetDateTime,
}

pub struct SystemSettingsUpdate<'a> {
    pub registration_open: bool,
    pub require_email_verification: bool,
    pub max_sessions: i16,
    pub maintenance_enabled: bool,
    pub maintenance_message: &'a str,
    pub updated_by: i32,
}

pub async fn get(pool: &DbPool) -> Result<SystemSettings, RbInternalError> {
    Ok(sqlx::query_as!(
        SystemSettings,
        "SELECT registration_open, require_email_verification, max_sessions,
            maintenance_enabled, maintenance_message, updated_by, updated_at
        FROM rb_system_settings WHERE singleton = TRUE"
    )
    .fetch_one(pool)
    .await?)
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
            max_sessions = $3,
            maintenance_enabled = $4,
            maintenance_message = $5,
            updated_by = $6,
            updated_at = CURRENT_TIMESTAMP
        WHERE singleton = TRUE
        RETURNING registration_open, require_email_verification, max_sessions,
            maintenance_enabled, maintenance_message, updated_by, updated_at",
        data.registration_open,
        data.require_email_verification,
        data.max_sessions,
        data.maintenance_enabled,
        data.maintenance_message,
        data.updated_by,
    )
    .fetch_one(pool)
    .await?)
}
