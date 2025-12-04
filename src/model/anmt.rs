use serde::Serialize;
use sqlx::{prelude::FromRow, types::time::OffsetDateTime};

#[derive(FromRow, Serialize)]
pub struct RbAnmt {
    pub id: i32,
    pub title: String,
    pub content: String,
    pub is_pinned: bool,
    pub is_shown: bool,
    pub game_id: Option<i32>,
    pub ctime_at: OffsetDateTime,
}
