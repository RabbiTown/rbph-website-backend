use deadpool_redis::redis::{AsyncCommands, Script};
use sha2::{Digest, Sha256};

use crate::{KvPool, error::RbInternalError};

const CONSUME_SCRIPT: &str = r#"
local count = redis.call('INCR', KEYS[1])
if count == 1 then
    redis.call('EXPIRE', KEYS[1], ARGV[1])
end
return {count, redis.call('TTL', KEYS[1])}
"#;

pub fn key(scope: &str, value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!("auth_rate:v1:{scope}:{digest:x}")
}

pub async fn consume(
    pool: &KvPool,
    key: &str,
    limit: u64,
    window_seconds: u64,
) -> Result<Option<u64>, RbInternalError> {
    let mut conn = pool.get().await?;
    let (count, ttl): (u64, i64) = Script::new(CONSUME_SCRIPT)
        .key(key)
        .arg(window_seconds.max(1))
        .invoke_async(&mut conn)
        .await?;

    Ok((count > limit).then_some(ttl.max(1) as u64))
}

pub async fn blocked(pool: &KvPool, key: &str, limit: u64) -> Result<Option<u64>, RbInternalError> {
    let mut conn = pool.get().await?;
    let count: Option<u64> = conn.get(key).await?;
    if count.unwrap_or_default() < limit {
        return Ok(None);
    }

    let ttl: i64 = conn.ttl(key).await?;
    Ok(Some(ttl.max(1) as u64))
}

pub async fn clear(pool: &KvPool, key: &str) -> Result<(), RbInternalError> {
    let mut conn = pool.get().await?;
    let _: () = conn.del(key).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::key;

    #[test]
    fn keys_do_not_expose_identifiers() {
        let key = key("login_ip_email", "127.0.0.1\0user@example.com");
        assert!(key.starts_with("auth_rate:v1:login_ip_email:"));
        assert!(!key.contains("user@example.com"));
        assert!(!key.contains("127.0.0.1"));
        assert_eq!(
            key,
            super::key("login_ip_email", "127.0.0.1\0user@example.com")
        );
        assert_ne!(
            key,
            super::key("login_ip_email", "127.0.0.2\0user@example.com")
        );
    }
}
