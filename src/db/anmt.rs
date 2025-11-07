use sqlx::QueryBuilder;

use crate::{DbPool, error::RbInternalError, model::anmt::RbAnmt};

pub struct RbAnmtPutData<'a> {
    pub title: &'a str,
    pub content: &'a str,
    pub pinned: bool,
    pub shown: bool,
    pub game_id: Option<i32>,
}

pub async fn append(pool: &DbPool, data: &RbAnmtPutData<'_>) -> Result<i32, RbInternalError> {
    let result = sqlx::query_scalar!(
        "INSERT INTO rb_anmt (title, content, pinned, shown, game_id)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id;",
        data.title,
        data.content,
        data.pinned,
        data.shown,
        data.game_id
    )
    .fetch_one(pool)
    .await?;

    Ok(result)
}

pub async fn get(pool: &DbPool, id: i32) -> Result<Option<RbAnmt>, RbInternalError> {
    let ret = sqlx::query_as!(RbAnmt, "SELECT * FROM rb_anmt WHERE id = $1;", id)
        .fetch_one(pool)
        .await;

    match ret {
        Ok(result) => Ok(Some(result)),
        Err(sqlx::Error::RowNotFound) => Ok(None),
        Err(err) => Err(RbInternalError::Sql(err)),
    }
}

pub async fn list_all(
    pool: &DbPool,
    only_shown: bool,
    game_id: Option<i32>,
) -> Result<Vec<RbAnmt>, RbInternalError> {
    let mut qb = QueryBuilder::new("SELECT * FROM rb_anmt WHERE 1=1");

    if only_shown {
        qb.push(" AND shown = true");
    }

    if let Some(gid) = game_id {
        qb.push(" AND (game_id IS NULL OR game_id = ");
        qb.push_bind(gid);
        qb.push(")");
    }

    qb.push(" ORDER BY pinned DESC, ctime_at DESC;");

    let result = qb.build_query_as::<RbAnmt>().fetch_all(pool).await?;

    Ok(result)
}
