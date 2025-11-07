use num_enum::{FromPrimitive, IntoPrimitive};
use serde::Serialize;
use sqlx::{prelude::FromRow, types::time::OffsetDateTime};

#[derive(FromRow, Serialize)]
pub struct RbTeam {
    pub id: i32,
    pub tname: String,
    pub pass: String,
    pub bio: String,
    pub locked: bool,
    pub ctime_at: OffsetDateTime,
}
