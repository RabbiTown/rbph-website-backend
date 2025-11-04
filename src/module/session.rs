use actix_session::Session;
use deadpool_redis::redis::AsyncCommands;
use uuid::Uuid;

use crate::error::RbInternalError;

static USER_SESSIONS: &str = "user_sessions";

pub fn get_session_id(sess: &Session) -> Result<String, RbInternalError> {
    if let Ok(Some(id)) = sess.get::<String>("session_id") {
        Ok(id)
    } else {
        let new_id = Uuid::new_v4().to_string();
        sess.insert("session_id", &new_id)?;
        Ok(new_id)
    }
}

pub async fn put(
    pool: &deadpool_redis::Pool,
    sess: &Session,
    user_id: i32,
    max_session: usize,
) -> Result<(), RbInternalError> {
    let mut conn = pool.get().await?;

    let key = format!("{}:{}", USER_SESSIONS, user_id);

    let _: () = conn.lpush(&key, get_session_id(sess)?).await?;
    let _: () = conn.ltrim(&key, 0, (max_session - 1) as isize).await?;

    sess.insert("user_id", user_id)?;

    Ok(())
}

pub async fn verify(pool: &deadpool_redis::Pool, sess: &Session) -> Result<bool, RbInternalError> {
    let user_id = sess.get::<i32>("user_id").ok().flatten();
    let sid = sess.get::<String>("session_id").ok().flatten();

    match (user_id, sid) {
        (Some(user_id), Some(sid)) => {
            let mut conn = pool.get().await?;

            let key = format!("{}:{}", USER_SESSIONS, user_id);
            let sessions: Vec<String> = conn.lrange(&key, 0, -1).await.unwrap_or_default();

            Ok(sessions.contains(&sid))
        }
        _ => Ok(false),
    }
}

pub async fn invalidate(
    pool: &deadpool_redis::Pool,
    sess: &Session,
) -> Result<bool, RbInternalError> {
    let user_id = sess.get::<i32>("user_id").ok().flatten();
    let sid = sess.get::<String>("session_id").ok().flatten();

    match (user_id, sid) {
        (Some(user_id), Some(sid)) => {
            let mut conn = pool.get().await?;

            let key = format!("{}:{}", USER_SESSIONS, user_id);
            let count: i32 = conn.lrem(&key, 1, &sid).await?;

            Ok(count > 0)
        }
        _ => Ok(false),
    }
}

pub async fn invalidate_others(
    pool: &deadpool_redis::Pool,
    sess: &Session,
) -> Result<bool, RbInternalError> {
    let user_id = sess.get::<i32>("user_id").ok().flatten();
    let sid = sess.get::<String>("session_id").ok().flatten();

    match (user_id, sid) {
        (Some(user_id), Some(sid)) => {
            let mut conn = pool.get().await?;

            let key = format!("{}:{}", USER_SESSIONS, user_id);
            let _: () = conn.del(&key).await?;
            let _: () = conn.lpush(&key, &sid).await?;

            Ok(true)
        }
        _ => Ok(false),
    }
}
