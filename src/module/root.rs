use serde_json::json;

use crate::{
    DbPool,
    db::event_log::{self, EventLogInput, EventScope, EventSeverity},
    error::RbInternalError,
    model::user::RbUserRole,
};

const ROOT_ID: i32 = 0;
const ROOT_EMAIL: &str = "root@rbph.local";
const ROOT_NICKNAME: &str = "Root";

pub async fn ensure_root(pool: &DbPool) -> Result<bool, RbInternalError> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(1380077640)")
        .execute(&mut *tx)
        .await?;

    let existing_role = sqlx::query_scalar!("SELECT urole FROM rb_user WHERE id = $1", ROOT_ID)
        .fetch_optional(&mut *tx)
        .await?;
    if let Some(role) = existing_role {
        if role != i16::from(RbUserRole::Root) {
            return Err("User id 0 exists but is not Root".into());
        }
        tx.commit().await?;
        return Ok(false);
    }

    let password = std::env::var("RBPH_ROOT_PASSWORD")
        .map_err(|_| "RBPH_ROOT_PASSWORD is required to create the initial Root user")?;
    validate_password(&password)?;

    let email_owner = sqlx::query_scalar!("SELECT id FROM rb_user WHERE email = $1", ROOT_EMAIL)
        .fetch_optional(&mut *tx)
        .await?;
    if email_owner.is_some() {
        return Err(format!("Root email {ROOT_EMAIL} is already used by another user").into());
    }

    let password_hash = bcrypt::hash(password, 12)?;
    sqlx::query!(
        "INSERT INTO rb_user (id, email, pass, urole, nickname, must_change_password)
         VALUES ($1, $2, $3, $4, $5, FALSE)",
        ROOT_ID,
        ROOT_EMAIL,
        password_hash,
        i16::from(RbUserRole::Root),
        ROOT_NICKNAME,
    )
    .execute(&mut *tx)
    .await?;

    event_log::insert_tx(
        &mut tx,
        EventLogInput {
            event_type: "system.root_bootstrap",
            event_scope: i16::from(EventScope::Security),
            severity: i16::from(EventSeverity::Info),
            user_id: Some(ROOT_ID),
            data: json!({ "email": ROOT_EMAIL }),
            ..Default::default()
        },
    )
    .await?;

    tx.commit().await?;
    log::info!("Created initial Root user {ROOT_EMAIL} with id {ROOT_ID}");
    Ok(true)
}

fn validate_password(password: &str) -> Result<(), RbInternalError> {
    if password == "CHANGE_ME"
        || !(8..=64).contains(&password.len())
        || !password.bytes().all(|byte| (b'!'..=b'~').contains(&byte))
    {
        return Err(
            "RBPH_ROOT_PASSWORD must be 8-64 printable ASCII characters and not CHANGE_ME".into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_password;

    #[test]
    fn validates_root_password() {
        assert!(validate_password("strong-password").is_ok());
        assert!(validate_password("short").is_err());
        assert!(validate_password("CHANGE_ME").is_err());
        assert!(validate_password("contains space").is_err());
        assert!(validate_password("包含非ASCII").is_err());
    }
}
