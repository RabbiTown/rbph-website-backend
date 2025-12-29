mod api;
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

use actix::{Actor, Addr};
use actix_session::{SessionMiddleware, storage::RedisSessionStore};
use actix_web::{App, HttpServer, cookie::Key, middleware::Logger, web};
use dotenvy::dotenv;
use env_logger::Env;
use sqlx::PgPool;

use crate::{config::Settings, module::sync::SyncHub};

pub type DbPool = PgPool;
pub type KvPool = deadpool_redis::Pool;

pub struct AppState {
    pub db: DbPool,
    pub kv: KvPool,
    pub settings: Settings,
    pub sync_hub: Addr<SyncHub>,
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
    let config = settings.app.clone();

    let db_pool = db::create_pool(&config.db_addr).await.unwrap();

    let kv_pool = deadpool_redis::Config::from_url(&config.kv_addr)
        .create_pool(Some(deadpool_redis::Runtime::Tokio1))
        .unwrap();

    let session_store = RedisSessionStore::new(&config.kv_addr).await.unwrap();
    let secret_key = Key::from(&config.get_secret_key());

    let sync_hub = SyncHub::default().start();

    let app_state = AppState {
        db: db_pool,
        kv: kv_pool,
        settings,
        sync_hub,
    };
    let app_state_data = web::Data::new(app_state);

    let (host, port) = (&config.bind_addr.0, config.bind_addr.1);
    let server = HttpServer::new(move || {
        App::new()
            .app_data(app_state_data.clone())
            .wrap(Logger::default())
            .wrap(
                SessionMiddleware::builder(session_store.clone(), secret_key.clone())
                    .cookie_secure(config.production)
                    .cookie_http_only(true)
                    .cookie_same_site(actix_web::cookie::SameSite::Lax)
                    .cookie_name("rbph_session".to_string())
                    .build(),
            )
            .service(web::scope("/api").configure(api::config))
    })
    .bind((host.as_str(), port))?
    .run();

    log::info!(
        "Running on http://{}:{} ({})",
        host,
        port,
        if config.production {
            "PRODUCTION"
        } else {
            "DEVELOP"
        }
    );

    server.await
}
