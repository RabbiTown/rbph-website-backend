use sqlx::QueryBuilder;
use time::OffsetDateTime;

use crate::{DbPool, error::RbInternalError, model::game::RbGame};

pub struct RbGamePutData {
    pub title: String,
    pub reg_open_at: Option<OffsetDateTime>,
    pub pre_open_at: Option<OffsetDateTime>,
    pub start_at: OffsetDateTime,
    pub end_at: OffsetDateTime,
}

pub async fn append(pool: &DbPool, data: &RbGamePutData) -> Result<i32, RbInternalError> {
    let result = sqlx::query_scalar!(
        "INSERT INTO rb_game (title, reg_open_at, pre_open_at, start_at, end_at)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id;",
        data.title,
        data.reg_open_at,
        data.pre_open_at,
        data.start_at,
        data.end_at
    )
    .fetch_one(pool)
    .await?;

    Ok(result)
}

pub async fn list_all(pool: &DbPool, only_shown: bool) -> Result<Vec<RbGame>, RbInternalError> {
    let mut qb = QueryBuilder::new("SELECT * FROM rb_game WHERE 1=1");

    if only_shown {
        qb.push(" AND shown = true");
    }

    let result = qb.build_query_as::<RbGame>().fetch_all(pool).await?;

    Ok(result)
}
