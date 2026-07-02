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
}

#[derive(Deserialize, Clone)]
pub struct CaptchaConfig {}

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
