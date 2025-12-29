use serde::Serialize;
use sqlx::{prelude::FromRow, types::time::OffsetDateTime};

use crate::model::game::RbContentType;

#[derive(FromRow, Serialize)]
pub struct RbAnnouncement {
    pub id: i32,
    pub title: String,
    pub content: String,
    pub content_type: RbContentType,
    pub is_pinned: bool,
    pub is_shown: bool,
    pub game_id: Option<i32>,
    pub puzzle_id: Option<i32>,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub ctime_at: OffsetDateTime,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub utime_at: OffsetDateTime,
}
