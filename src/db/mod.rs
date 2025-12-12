pub mod anmt;
pub mod game;
pub mod team;
pub mod user;

use sqlx::postgres::PgPoolOptions;

use crate::DbPool;

pub async fn create_pool(url: &str) -> Result<DbPool, sqlx::Error> {
    let pool = PgPoolOptions::new().max_connections(5).connect(url).await?;
    // Run embedded sqlx migrations at startup so local/dev environments
    // get schema applied automatically. If migrations fail, return the error
    // to stop the server from starting.
    sqlx::migrate!().run(&pool).await?;
    Ok(pool)
}
