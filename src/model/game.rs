use num_enum::{FromPrimitive, IntoPrimitive};
use serde::Serialize;
use sqlx::{prelude::FromRow, types::time::OffsetDateTime};

#[derive(FromRow, Serialize)]
pub struct RbGame {
    pub id: i32,
    pub title: String,
    pub shown: bool,
    pub reg_open_at: OffsetDateTime,
    pub pre_open_at: OffsetDateTime,
    pub start_at: OffsetDateTime,
    pub end_at: OffsetDateTime,
}

impl RbGame {
    pub fn is_started(&self) -> bool {
        let now = OffsetDateTime::now_utc();
        now >= self.start_at
    }

    pub fn is_ended(&self) -> bool {
        let now = OffsetDateTime::now_utc();
        now >= self.end_at
    }
}

#[derive(FromPrimitive, IntoPrimitive, Serialize, Clone, Copy, Eq, PartialEq)]
#[repr(i16)]
pub enum RbGameEntryState {
    Banned = -1,
    InGame = 0,
    Finished = 1,

    #[num_enum(catch_all)]
    Invalid(i16),
}

#[derive(FromRow, Serialize)]
pub struct RbGameEntry {
    pub game_id: i32,
    pub team_id: i32,
    pub estate: RbGameEntryState,
    pub ctime_at: OffsetDateTime,
}
