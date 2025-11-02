mod api;
mod config;
mod db;
mod model;
mod module;

use actix_session::{SessionMiddleware, storage::RedisSessionStore};
use actix_web::{App, HttpServer, cookie::Key, middleware::Logger, web};
use dotenvy::dotenv;
use env_logger::Env;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    dotenv().ok();

    env_logger::init_from_env(Env::default().default_filter_or("info"));

    let settings = config::Settings::read_from_file("config.toml");
    if settings.is_err() {
        log::error!("failed to read config : {}", settings.err().unwrap());
        std::process::exit(1);
    }

    let settings = settings.unwrap();
    let config = settings.app.clone();

    let db_pool = db::create_pool(&config.db_addr).await.unwrap();
    let db_pool_data = web::Data::new(db_pool);

    let kv_pool = deadpool_redis::Config::from_url(&config.kv_addr)
        .create_pool(Some(deadpool_redis::Runtime::Tokio1))
        .unwrap();
    let kv_pool_data = web::Data::new(kv_pool);

    let session_store = RedisSessionStore::new(&config.kv_addr).await.unwrap();
    let secret_key = Key::from(&config.get_secret_key());

    let settings_data = web::Data::new(settings);

    let (host, port) = (&config.bind_addr.0, config.bind_addr.1);
    let server = HttpServer::new(move || {
        App::new()
            .app_data(db_pool_data.clone())
            .app_data(kv_pool_data.clone())
            .app_data(settings_data.clone())
            .wrap(Logger::default())
            .wrap(
                SessionMiddleware::builder(session_store.clone(), secret_key.clone())
                    .cookie_secure(config.production)
                    .cookie_http_only(true)
                    .cookie_same_site(actix_web::cookie::SameSite::Lax)
                    .cookie_name("rbph_session".to_string())
                    .build(),
            )
            .service(web::scope("/api/v1").configure(api::config))
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
