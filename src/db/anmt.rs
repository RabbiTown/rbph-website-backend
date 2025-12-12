use sqlx::QueryBuilder;

use crate::{DbPool, error::RbInternalError, model::anmt::RbAnnouncement};

pub struct RbAnnouncementPutData<'a> {
    pub title: &'a str,
    pub content: &'a str,
    pub is_pinned: bool,
    pub is_shown: bool,
    pub game_id: Option<i32>,
}

pub async fn append(
    pool: &DbPool,
    data: &RbAnnouncementPutData<'_>,
) -> Result<i32, RbInternalError> {
    let result = sqlx::query_scalar!(
        "INSERT INTO rb_announcement (title, content, is_pinned, is_shown, game_id)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id;",
        data.title,
        data.content,
        data.is_pinned,
        data.is_shown,
        data.game_id
    )
    .fetch_one(pool)
    .await?;

    Ok(result)
}

pub async fn get(pool: &DbPool, id: i32) -> Result<Option<RbAnnouncement>, RbInternalError> {
    let ret = sqlx::query_as!(
        RbAnnouncement,
        "SELECT * FROM rb_announcement WHERE id = $1;",
        id
    )
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
) -> Result<Vec<RbAnnouncement>, RbInternalError> {
    let mut qb = QueryBuilder::new("SELECT * FROM rb_announcement WHERE 1=1");

    if only_shown {
        qb.push(" AND is_shown = true");
    }

    if let Some(gid) = game_id {
        qb.push(" AND (game_id IS NULL OR game_id = ");
        qb.push_bind(gid);
        qb.push(")");
    }

    qb.push(" ORDER BY is_pinned DESC, ctime_at DESC;");

    let result = qb
        .build_query_as::<RbAnnouncement>()
        .fetch_all(pool)
        .await?;

    Ok(result)
}
