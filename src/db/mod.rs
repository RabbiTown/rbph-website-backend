use sqlx::{PgPool, postgres::PgPoolOptions};

pub mod user;

pub async fn create_pool(url: &str) -> Result<PgPool, sqlx::Error> {
    let pool = PgPoolOptions::new().max_connections(5).connect(url).await?;
    sqlx::migrate!();
    Ok(pool)
}
