pub mod anmt;
pub mod asset;
pub mod board;
pub mod cache;
pub mod content;
pub mod event_log;
pub mod feature;
pub mod frontend;
pub mod game;
pub mod notification;
pub mod puzzle;
pub mod puzzle_backend;
pub mod release;
pub mod round;
pub mod system_settings;
pub mod team;
pub mod ticket;
pub mod user;

use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use crate::{DbPool, config::DbConfig};

pub async fn create_pool(config: &DbConfig) -> Result<DbPool, sqlx::Error> {
    let mut options = PgConnectOptions::from_str(&config.addr)?;
    if let Some(password) = config
        .password
        .as_deref()
        .filter(|password| !password.is_empty())
    {
        options = options.password(password);
    }

    let pool = PgPoolOptions::new()
        .max_connections(config.max_connections)
        .connect_with(options)
        .await?;
    Ok(pool)
}
