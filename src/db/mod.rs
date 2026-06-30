pub mod anmt;
pub mod asset;
pub mod board;
pub mod cache;
pub mod event_log;
pub mod feature;
pub mod game;
pub mod notification;
pub mod puzzle;
pub mod puzzle_backend;
pub mod release;
pub mod round;
pub mod team;
pub mod ticket;
pub mod user;

use sqlx::postgres::PgPoolOptions;

use crate::DbPool;

pub async fn create_pool(url: &str, max_connections: u32) -> Result<DbPool, sqlx::Error> {
    let pool = PgPoolOptions::new()
        .max_connections(max_connections)
        .connect(url)
        .await?;
    Ok(pool)
}
