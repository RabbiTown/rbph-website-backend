use base64::{Engine, prelude::BASE64_STANDARD};
use std::collections::BTreeMap;

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
    pub fn get_secret_key(&self) -> Result<[u8; 64], String> {
        let decoded = BASE64_STANDARD
            .decode(&self.secret_key)
            .map_err(|_| "app.secret_key must be valid Base64")?;
        if decoded.len() != 64 {
            return Err("app.secret_key must decode to exactly 64 bytes".to_string());
        }
        let mut key = [0u8; 64];
        key.copy_from_slice(&decoded);
        Ok(key)
    }
}

#[derive(Deserialize, Clone)]
pub struct DbConfig {
    pub addr: String,
    pub password: Option<String>,
    #[serde(default = "default_db_max_connections")]
    pub max_connections: u32,
}

const fn default_db_max_connections() -> u32 {
    32
}

#[derive(Deserialize, Clone)]
pub struct StorageConfig {
    pub default_backend: String,
    #[serde(default)]
    pub content_cdn_backend: Option<String>,
    pub backends: BTreeMap<String, StorageBackendConfig>,
}

#[derive(Deserialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StorageBackendConfig {
    Local {
        label: String,
        asset_root: String,
    },
    Cos {
        label: String,
        region: String,
        bucket: String,
        secret_id: String,
        secret_key: String,
        public_base_url: String,
    },
}

impl StorageConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !self.backends.contains_key(&self.default_backend) {
            return Err("storage.default_backend must reference a configured backend".to_string());
        }
        if let Some(backend) = &self.content_cdn_backend
            && !self.backends.contains_key(backend)
        {
            return Err(
                "storage.content_cdn_backend must reference a configured backend".to_string(),
            );
        }
        if !self
            .backends
            .values()
            .any(|backend| matches!(backend, StorageBackendConfig::Local { .. }))
        {
            return Err("at least one local storage backend must be configured".to_string());
        }

        for (id, backend) in &self.backends {
            if !valid_storage_backend_id(id) {
                return Err(format!("invalid storage backend id: {id}"));
            }
            let label = match backend {
                StorageBackendConfig::Local { label, asset_root } => {
                    if asset_root.trim().is_empty() {
                        return Err(format!("storage backend {id} has an empty asset_root"));
                    }
                    label
                }
                StorageBackendConfig::Cos {
                    label,
                    region,
                    bucket,
                    secret_id,
                    secret_key,
                    public_base_url,
                } => {
                    if region.trim().is_empty()
                        || bucket.trim().is_empty()
                        || secret_id.trim().is_empty()
                        || secret_key.trim().is_empty()
                        || !public_base_url.starts_with("https://")
                    {
                        return Err(format!("storage backend {id} has invalid COS settings"));
                    }
                    label
                }
            };
            if label.trim().is_empty() || label.chars().count() > 64 {
                return Err(format!("storage backend {id} has an invalid label"));
            }
        }
        Ok(())
    }
}

fn valid_storage_backend_id(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(ch) if ch.is_ascii_alphabetic())
        && value.len() <= 32
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
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
            .add_source(config::File::with_name("config.local.toml").required(false))
            .add_source(config::Environment::with_prefix("RBPH").separator("__"))
            .build()?;
        cfg.try_deserialize()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{AppConfig, AuthRateLimitConfig, StorageBackendConfig, StorageConfig};

    #[test]
    fn session_secret_requires_exactly_64_bytes() {
        let valid = AppConfig {
            production: true,
            bind_addr: ("127.0.0.1".to_string(), 9999),
            kv_addr: "redis://localhost/1".to_string(),
            secret_key: base64::Engine::encode(&base64::prelude::BASE64_STANDARD, [7u8; 64]),
        };
        assert!(valid.get_secret_key().is_ok());

        let invalid = AppConfig {
            secret_key: "c2hvcnQ=".to_string(),
            ..valid
        };
        assert!(invalid.get_secret_key().is_err());
    }

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

    #[test]
    fn named_storage_requires_valid_default_and_local_backend() {
        let mut backends = BTreeMap::new();
        backends.insert(
            "local".to_string(),
            StorageBackendConfig::Local {
                label: "Local".to_string(),
                asset_root: "./assets".to_string(),
            },
        );
        let valid = StorageConfig {
            default_backend: "local".to_string(),
            content_cdn_backend: None,
            backends,
        };
        assert!(valid.validate().is_ok());

        let invalid_default = StorageConfig {
            default_backend: "missing".to_string(),
            content_cdn_backend: None,
            backends: valid.backends.clone(),
        };
        assert!(invalid_default.validate().is_err());

        let invalid_content_cdn = StorageConfig {
            default_backend: "local".to_string(),
            content_cdn_backend: Some("missing".to_string()),
            backends: valid.backends.clone(),
        };
        assert!(invalid_content_cdn.validate().is_err());

        let cos_only = StorageConfig {
            default_backend: "cos".to_string(),
            content_cdn_backend: None,
            backends: BTreeMap::from([(
                "cos".to_string(),
                StorageBackendConfig::Cos {
                    label: "COS".to_string(),
                    region: "ap-shanghai".to_string(),
                    bucket: "example-1234567890".to_string(),
                    secret_id: "id".to_string(),
                    secret_key: "key".to_string(),
                    public_base_url: "https://assets.example.com".to_string(),
                },
            )]),
        };
        assert!(cos_only.validate().is_err());

        let mut invalid_id = valid;
        invalid_id.backends.insert(
            "cos/provider".to_string(),
            StorageBackendConfig::Cos {
                label: "COS".to_string(),
                region: "ap-shanghai".to_string(),
                bucket: "example-1234567890".to_string(),
                secret_id: "id".to_string(),
                secret_key: "key".to_string(),
                public_base_url: "https://assets.example.com".to_string(),
            },
        );
        assert!(invalid_id.validate().is_err());

        let mut invalid_cos = invalid_id;
        invalid_cos.backends.remove("cos/provider");
        invalid_cos.backends.insert(
            "cos".to_string(),
            StorageBackendConfig::Cos {
                label: "COS".to_string(),
                region: "ap-shanghai".to_string(),
                bucket: "example-1234567890".to_string(),
                secret_id: "id".to_string(),
                secret_key: "key".to_string(),
                public_base_url: "http://assets.example.com".to_string(),
            },
        );
        assert!(invalid_cos.validate().is_err());
    }
}
