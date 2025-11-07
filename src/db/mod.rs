pub mod anmt;
pub mod game;
pub mod team;
pub mod user;

use sqlx::postgres::PgPoolOptions;

use crate::DbPool;

pub async fn create_pool(url: &str) -> Result<DbPool, sqlx::Error> {
    let pool = PgPoolOptions::new().max_connections(5).connect(url).await?;
    sqlx::migrate!();
    Ok(pool)
}
