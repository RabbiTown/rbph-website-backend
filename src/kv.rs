use std::{ops::Deref, sync::Arc};

use deadpool_redis::{Pool, redis};

pub const SESSION_KEY_VERSION: u8 = 1;

#[derive(Clone)]
pub struct KvStore {
    pool: Pool,
    redis_url: Arc<str>,
    prefix: Arc<str>,
}

impl KvStore {
    pub fn new(pool: Pool, redis_url: impl Into<Arc<str>>, deployment_id: &str) -> Self {
        Self {
            pool,
            redis_url: redis_url.into(),
            prefix: format!("rbph:{deployment_id}:").into(),
        }
    }

    pub fn key(&self, logical_key: impl AsRef<str>) -> String {
        format!("{}{}", self.prefix, logical_key.as_ref())
    }

    pub fn pattern(&self, logical_pattern: impl AsRef<str>) -> String {
        self.key(logical_pattern)
    }

    pub fn channel(&self, logical_channel: impl AsRef<str>) -> String {
        self.key(format!("channel:{}", logical_channel.as_ref()))
    }

    pub fn session_key(&self, session_key: &str) -> String {
        self.key(format!("session:v{SESSION_KEY_VERSION}:data:{session_key}"))
    }

    pub fn redis_client(&self) -> redis::RedisResult<redis::Client> {
        redis::Client::open(self.redis_url.as_ref())
    }
}

impl Deref for KvStore {
    type Target = Pool;

    fn deref(&self) -> &Self::Target {
        &self.pool
    }
}

#[cfg(test)]
mod tests {
    use super::KvStore;

    fn store(deployment_id: &str) -> KvStore {
        let pool = deadpool_redis::Config::from_url("redis://127.0.0.1/15")
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .expect("test Redis pool configuration should be valid");
        KvStore::new(pool, "redis://127.0.0.1/15", deployment_id)
    }

    #[test]
    fn namespaces_keys_patterns_channels_and_sessions() {
        let first = store("production");
        let second = store("staging");

        assert_eq!(
            first.key("cache:leaderboard:v1:game:1"),
            "rbph:production:cache:leaderboard:v1:game:1"
        );
        assert_eq!(
            first.pattern("cache:puzzle-hints:v1:*"),
            "rbph:production:cache:puzzle-hints:v1:*"
        );
        assert_eq!(first.channel("sync:v1"), "rbph:production:channel:sync:v1");
        assert_eq!(
            first.session_key("opaque"),
            "rbph:production:session:v1:data:opaque"
        );
        assert_ne!(first.key("same"), second.key("same"));
    }
}
