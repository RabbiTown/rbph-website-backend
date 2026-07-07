pub mod api;
pub mod asset;
pub mod config;
pub mod db;
pub mod error;
pub mod expr;
pub mod extractor;
pub mod game;
pub mod health;
pub mod middleware;
pub mod model;
pub mod module;
pub mod serde_helpers;

use std::sync::Arc;

use sqlx::PgPool;
use tokio::sync::{Notify, RwLock};

use crate::{
    config::Settings,
    module::{
        captcha::CaptchaService, email::EmailService, storage::StorageManager, sync::SyncHub,
    },
};

pub type DbPool = PgPool;
pub type KvPool = deadpool_redis::Pool;

#[derive(Clone)]
pub struct AppState {
    pub db: DbPool,
    pub kv: KvPool,
    pub settings: Settings,
    pub system_settings: Arc<RwLock<db::system_settings::SystemSettings>>,
    pub sync_hub: Arc<SyncHub>,
    pub release_schedule_changed: Arc<Notify>,
    pub captcha: Option<Arc<CaptchaService>>,
    pub email: Option<Arc<EmailService>>,
    pub storage: StorageManager,
}
