pub mod api;
pub mod asset;
pub mod config;
pub mod db;
pub mod error;
pub mod expr;
pub mod extractor;
pub mod game;
pub mod health;
pub mod kv;
pub mod middleware;
pub mod model;
pub mod module;
pub mod serde_helpers;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub fn embedded_schema_generation() -> i64 {
    MIGRATOR
        .iter()
        .map(|migration| migration.version)
        .max()
        .expect("at least one embedded migration is required")
}

use std::sync::Arc;

use sqlx::PgPool;
use tokio::sync::{Notify, RwLock};

use crate::{
    config::Settings,
    kv::KvStore,
    module::{
        captcha::CaptchaService, cluster::ClusterMembership, email::EmailService,
        storage::StorageManager, sync::SyncHub,
    },
};

pub type DbPool = PgPool;

#[derive(Clone)]
pub struct AppState {
    pub db: DbPool,
    pub kv: KvStore,
    pub settings: Settings,
    pub cluster_membership: Arc<ClusterMembership>,
    pub system_settings: Arc<RwLock<db::system_settings::SystemSettings>>,
    pub sync_hub: Arc<SyncHub>,
    pub release_schedule_changed: Arc<Notify>,
    pub captcha: Option<Arc<CaptchaService>>,
    pub email: Option<Arc<EmailService>>,
    pub storage: StorageManager,
}
