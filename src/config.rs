use base64::{Engine, prelude::BASE64_STANDARD};
use config::Config;
use serde::Deserialize;

#[derive(Deserialize, Clone)]
pub struct AppConfig {
    pub production: bool,

    pub bind_addr: (String, u16),
    pub kv_addr: String,

    pub secret_key: String,
}

impl AppConfig {
    pub fn get_secret_key(&self) -> [u8; 64] {
        let decoded = BASE64_STANDARD.decode(&self.secret_key);

        if decoded.is_err() {
            log::warn!("invalid secret key found, default to zero bytes.")
        }

        let decoded = decoded.unwrap_or_default();

        let mut key = [0u8; 64];
        let len = decoded.len().min(64);
        key[..len].copy_from_slice(&decoded[..len]);

        key
    }
}

#[derive(Deserialize, Clone)]
pub struct DbConfig {
    pub addr: String,
    #[serde(default = "default_db_max_connections")]
    pub max_connections: u32,
}

const fn default_db_max_connections() -> u32 {
    32
}

#[derive(Deserialize, Clone)]
pub struct StorageConfig {
    #[serde(default = "default_storage_kind")]
    pub kind: String,
    pub asset_root: String,
}

fn default_storage_kind() -> String {
    "local".to_string()
}

#[derive(Deserialize, Clone)]
pub struct AuthConfig {
    pub captcha: CaptchaConfig,
    pub email: EmailConfig,
    #[serde(default)]
    pub rate_limit: AuthRateLimitConfig,
}

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct AuthRateLimitConfig {
    pub enabled: bool,
    pub login_ip_email_failures: u64,
    pub login_ip_attempts: u64,
    pub login_window_seconds: u64,
    pub registration_email_attempts: u64,
    pub registration_ip_attempts: u64,
    pub registration_window_seconds: u64,
}

impl Default for AuthRateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            login_ip_email_failures: 5,
            login_ip_attempts: 30,
            login_window_seconds: 15 * 60,
            registration_email_attempts: 3,
            registration_ip_attempts: 10,
            registration_window_seconds: 60 * 60,
        }
    }
}

impl AuthRateLimitConfig {
    pub fn is_valid(&self) -> bool {
        !self.enabled
            || (self.login_ip_email_failures > 0
                && self.login_ip_attempts > 0
                && self.login_window_seconds > 0
                && self.registration_email_attempts > 0
                && self.registration_ip_attempts > 0
                && self.registration_window_seconds > 0)
    }
}

#[derive(Deserialize, Clone)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum CaptchaConfig {
    Disabled,
    Cloudflare {
        site_key: String,
        secret_key: String,
        allowed_hostnames: Vec<String>,
        #[serde(default)]
        test_mode: bool,
    },
}

#[derive(Deserialize, Clone)]
pub struct Settings {
    pub app: AppConfig,
    pub db: DbConfig,
    pub storage: StorageConfig,
    pub auth: AuthConfig,
}

#[derive(Deserialize, Clone)]
pub struct EmailConfig {
    pub enabled: bool,
    pub sender: String,
    pub smtp: String,
    pub smtp_user: String,
    pub smtp_pass: String,
    pub url: UrlConfig,
}

#[derive(Deserialize, Clone)]
pub struct UrlConfig {
    pub verify: String,
}

impl Settings {
    pub fn read_from_file(file: &str) -> Result<Self, config::ConfigError> {
        let cfg = Config::builder()
            .add_source(config::File::with_name(file).required(true))
            .add_source(config::Environment::with_prefix("RBPH").separator("__"))
            .build()?;
        cfg.try_deserialize()
    }
}

#[cfg(test)]
mod tests {
    use super::AuthRateLimitConfig;

    #[test]
    fn auth_rate_limit_defaults_are_balanced() {
        let config = AuthRateLimitConfig::default();
        assert!(config.enabled);
        assert_eq!(config.login_ip_email_failures, 5);
        assert_eq!(config.login_ip_attempts, 30);
        assert_eq!(config.login_window_seconds, 900);
        assert_eq!(config.registration_email_attempts, 3);
        assert_eq!(config.registration_ip_attempts, 10);
        assert_eq!(config.registration_window_seconds, 3600);
        assert!(config.is_valid());
    }

    #[test]
    fn enabled_auth_rate_limit_rejects_zero_values() {
        let config = AuthRateLimitConfig {
            login_ip_email_failures: 0,
            ..Default::default()
        };
        assert!(!config.is_valid());

        let disabled = AuthRateLimitConfig {
            enabled: false,
            ..config
        };
        assert!(disabled.is_valid());
    }
}
