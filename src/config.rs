use base64::{Engine, prelude::BASE64_STANDARD};
use config::Config;
use log::warn;
use serde::Deserialize;

#[derive(Deserialize, Clone)]
pub struct AppConfig {
    pub production: bool,

    pub bind_addr: (String, u16),
    pub db_addr: String,
    pub kv_addr: String,

    pub secret_key: String,
}

impl AppConfig {
    pub fn get_secret_key(&self) -> [u8; 64] {
        let decoded = BASE64_STANDARD.decode(&self.secret_key);

        if decoded.is_err() {
            warn!("invalid secret key found, default to zero bytes.")
        }

        let decoded = decoded.unwrap_or_default();

        let mut key = [0u8; 64];
        let len = decoded.len().min(64);
        key[..len].copy_from_slice(&decoded[..len]);

        key
    }
}

#[derive(Deserialize)]
pub struct AuthConfig {
    pub max_session: usize,
    pub captcha: CaptchaConfig,
}

#[derive(Deserialize)]
pub struct CaptchaConfig {}

#[derive(Deserialize)]
pub struct Settings {
    pub app: AppConfig,
    pub auth: AuthConfig,
}

impl Settings {
    pub fn read_from_file(file: &str) -> Result<Self, config::ConfigError> {
        let cfg = Config::builder()
            .add_source(config::File::with_name(&file).required(true))
            .add_source(config::Environment::with_prefix("RBPH").separator("__"))
            .build()?;
        Ok(cfg.try_deserialize()?)
    }
}

// impl Default for AppConfig {
//     fn default() -> Self {
//         Self {
//             production: false,

//             bind_addr: ("localhost", 9999),
//             db_addr: "postgres://postgres:123456@localhost/rbph",
//             kv_addr: "redis://localhost/",

//             secret_key: b"\x12",
//         }
//     }
// }

// impl Default for AuthConfig {
//     fn default() -> Self {
//         Self {
//             max_session: 3,
//             captcha: None,
//         }
//     }
// }
