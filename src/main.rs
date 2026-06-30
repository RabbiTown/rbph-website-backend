mod api;
mod asset;
mod config;
mod db;
mod error;
mod expr;
mod extractor;
mod game;
mod middleware;
mod model;
mod module;
mod serde_helpers;

use std::{sync::Arc, time::Duration};

use actix_session::{SessionMiddleware, storage::RedisSessionStore};
use actix_web::{App, HttpServer, cookie::Key, middleware::Logger, web};
use dotenvy::dotenv;
use env_logger::Env;
use sqlx::PgPool;
use tokio::{sync::Notify, time};

use crate::{
    config::Settings,
    module::{email::EmailService, sync::SyncHub},
};

pub type DbPool = PgPool;
pub type KvPool = deadpool_redis::Pool;

#[derive(Clone)]
pub struct AppState {
    pub db: DbPool,
    pub kv: KvPool,
    pub settings: Settings,
    pub sync_hub: Arc<SyncHub>,
    pub release_schedule_changed: Arc<Notify>,
    pub email: Option<Arc<EmailService>>,
    pub storage: module::storage::LocalStorage,
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    dotenv().ok();

    env_logger::init_from_env(Env::default().default_filter_or("info"));

    let settings = config::Settings::read_from_file("config.toml");
    if settings.is_err() {
        log::error!("Failed to read config : {}", settings.err().unwrap());
        std::process::exit(1);
    }

    let settings = settings.unwrap();
    let app_config = settings.app.clone();
    let db_config = settings.db.clone();
    let storage_config = settings.storage.clone();

    let db_pool = db::create_pool(&db_config.addr, db_config.max_connections)
        .await
        .unwrap();

    let kv_pool = deadpool_redis::Config::from_url(&app_config.kv_addr)
        .create_pool(Some(deadpool_redis::Runtime::Tokio1))
        .unwrap();

    let session_store = RedisSessionStore::new(&app_config.kv_addr).await.unwrap();
    let secret_key = Key::from(&app_config.get_secret_key());

    let sync_hub = Arc::new(SyncHub::default());
    let release_schedule_changed = Arc::new(Notify::new());

    let email_service = if settings.auth.email.enabled {
        Some(Arc::new(
            EmailService::new(
                &settings.auth.email.smtp,
                &settings.auth.email.smtp_user,
                &settings.auth.email.smtp_pass,
                &settings.auth.email.sender,
            )
            .unwrap(),
        ))
    } else {
        None
    };

    let storage = module::storage::LocalStorage::new(storage_config.asset_root.clone());

    let app_state = AppState {
        db: db_pool,
        kv: kv_pool,
        settings,
        sync_hub: sync_hub.clone(),
        release_schedule_changed: release_schedule_changed.clone(),
        email: email_service.clone(),
        storage,
    };
    let app_state_data = web::Data::new(app_state);

    if let Err(error) = module::release::process_due_releases(app_state_data.get_ref()).await {
        log::error!("failed to initialize release scheduler: {error}");
    }

    let (host, port) = (&app_config.bind_addr.0, app_config.bind_addr.1);
    let http_app_state = app_state_data.clone();
    let server = HttpServer::new(move || {
        App::new()
            .app_data(http_app_state.clone())
            .wrap(Logger::default())
            .wrap(
                SessionMiddleware::builder(session_store.clone(), secret_key.clone())
                    .cookie_secure(app_config.production)
                    .cookie_http_only(true)
                    .cookie_same_site(actix_web::cookie::SameSite::Lax)
                    .cookie_name("rbph_session".to_string())
                    .build(),
            )
            .configure(asset::config)
            .service(web::scope("/api").configure(api::config))
    })
    .bind((host.as_str(), port))?
    .run();

    {
        let hub = sync_hub.clone();
        tokio::spawn(async move {
            let mut interval = time::interval(Duration::from_secs(30));
            interval.tick().await;
            loop {
                interval.tick().await;
                hub.cleanup();
            }
        });
    }

    {
        let state = app_state_data.get_ref().clone();
        let changed = release_schedule_changed.clone();
        tokio::spawn(module::release::run_scheduler(state, changed));
    }

    log::info!(
        "Running on http://{}:{} ({})",
        host,
        port,
        if app_config.production {
            "PRODUCTION"
        } else {
            "DEVELOP"
        }
    );

    server.await
}
