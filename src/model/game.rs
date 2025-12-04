use num_enum::{FromPrimitive, IntoPrimitive};
use serde::{Deserialize, Serialize};
use sqlx::{prelude::FromRow, types::time::OffsetDateTime};

#[derive(FromRow, Serialize)]
pub struct RbGame {
    pub id: i32,
    pub title: String,
    pub is_shown: bool,
    pub is_online: bool,
    pub reg_open_at: Option<OffsetDateTime>,
    pub pre_open_at: Option<OffsetDateTime>,
    pub start_at: OffsetDateTime,
    pub end_at: OffsetDateTime,
    pub ctime_at: OffsetDateTime,
    pub cover: Option<String>,
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

#[derive(Serialize, Deserialize, FromPrimitive, IntoPrimitive, Clone, Copy, Eq, PartialEq)]
#[repr(i16)]
#[serde(into = "i16")]
pub enum RbTeamState {
    Banned = -1,
    Open = 0,
    InGame = 1,
    Finished = 2,

    #[num_enum(catch_all)]
    Invalid(i16),
}

#[derive(FromRow, Serialize)]
pub struct RbTeam {
    pub id: i32,
    pub tname: String,
    pub tstate: RbTeamState,
    pub pass: String,
    pub bio: String,
    pub game_id: i32,
    pub ctime_at: OffsetDateTime,
}
