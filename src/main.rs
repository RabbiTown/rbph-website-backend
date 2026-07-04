mod api;
mod asset;
mod config;
mod db;
mod error;
mod expr;
mod extractor;
mod game;
mod health;
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
use tokio::{
    sync::{Notify, RwLock},
    time,
};

use crate::{
    config::Settings,
    middleware::maintenance::MaintenanceMiddleware,
    module::{captcha::CaptchaService, email::EmailService, sync::SyncHub},
};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

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
    if !settings.auth.rate_limit.is_valid() {
        log::error!("Invalid auth rate limit configuration: enabled limits must be positive");
        std::process::exit(1);
    }
    let app_config = settings.app.clone();
    let db_config = settings.db.clone();
    let storage_config = settings.storage.clone();

    let db_pool = db::create_pool(&db_config).await.unwrap_or_else(|error| {
        log::error!("Failed to connect to PostgreSQL: {error}");
        std::process::exit(1);
    });
    MIGRATOR.run(&db_pool).await.unwrap_or_else(|error| {
        log::error!("Failed to run database migrations: {error}");
        std::process::exit(1);
    });
    module::root::ensure_root(&db_pool)
        .await
        .unwrap_or_else(|error| {
            log::error!("Failed to initialize Root user: {error:?}");
            std::process::exit(1);
        });

    let kv_pool = deadpool_redis::Config::from_url(&app_config.kv_addr)
        .create_pool(Some(deadpool_redis::Runtime::Tokio1))
        .unwrap();

    let session_store = RedisSessionStore::new(&app_config.kv_addr).await.unwrap();
    let secret_key_bytes = match app_config.get_secret_key() {
        Ok(key) => key,
        Err(error) => {
            log::error!("Invalid session secret: {error}");
            std::process::exit(1);
        }
    };
    let secret_key = Key::from(&secret_key_bytes);

    let sync_hub = Arc::new(SyncHub::default());
    let release_schedule_changed = Arc::new(Notify::new());
    let captcha = match CaptchaService::from_config(&settings.auth.captcha) {
        Ok(service) => service.map(Arc::new),
        Err(error) => {
            log::error!("Invalid captcha configuration: {error}");
            std::process::exit(1);
        }
    };

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
    let previous_system_settings = db::system_settings::get(&db_pool).await.unwrap();
    let current_system_settings = db::system_settings::disable_unavailable_auth_features(
        &db_pool,
        captcha.is_some(),
        email_service.is_some(),
    )
    .await
    .unwrap();
    if previous_system_settings.require_email_verification
        && !current_system_settings.require_email_verification
    {
        log::warn!("Email verification was disabled because email delivery is not configured");
    }
    if (previous_system_settings.captcha_login_required
        || previous_system_settings.captcha_registration_required)
        && !current_system_settings.captcha_login_required
        && !current_system_settings.captcha_registration_required
    {
        log::warn!("Captcha requirements were disabled because captcha is not configured");
    }
    let system_settings = Arc::new(RwLock::new(current_system_settings));

    let storage = module::storage::LocalStorage::new(storage_config.asset_root.clone());

    let app_state = AppState {
        db: db_pool,
        kv: kv_pool,
        settings,
        system_settings,
        sync_hub: sync_hub.clone(),
        release_schedule_changed: release_schedule_changed.clone(),
        captcha,
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
            .configure(health::config)
            .configure(asset::config)
            .service(
                web::scope("/api")
                    .wrap(MaintenanceMiddleware)
                    .configure(api::config),
            )
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
