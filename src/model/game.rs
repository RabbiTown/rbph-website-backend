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
    pub intro_puzzle: Option<i32>,
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

#[derive(Serialize, Deserialize, FromPrimitive, IntoPrimitive, Clone, Copy, Eq, PartialEq)]
#[repr(i16)]
#[serde(into = "i16")]
pub enum RbPuzzleType {
    Normal = 0,
    Story = 1,

    #[num_enum(catch_all)]
    Invalid(i16),
}

#[derive(Serialize, Deserialize, FromPrimitive, IntoPrimitive, Clone, Copy, Eq, PartialEq)]
#[repr(i16)]
#[serde(into = "i16")]
pub enum RbContentType {
    Markdown = 0,
    Html = 1,

    #[num_enum(catch_all)]
    Invalid(i16),
}

#[derive(FromRow, Serialize)]
pub struct RbPuzzle {
    pub id: i32,
    pub title: String,
    pub ptype: RbPuzzleType,
    pub content: String,
    pub content_type: RbContentType,
    pub judge: String,
    pub unlock_cond: String,
    pub round_id: i32,
    pub ctime_at: OffsetDateTime,
}

#[derive(Serialize, Deserialize, FromPrimitive, IntoPrimitive, Clone, Copy, Eq, PartialEq)]
#[repr(i16)]
#[serde(into = "i16")]
pub enum RbTeamPuzzleState {
    Locked = -1,
    Unlocked = 0,
    Solved = 1,

    #[num_enum(catch_all)]
    Invalid(i16),
}

impl RbTeamPuzzleState {
    pub fn accessible(&self) -> bool {
        matches!(
            self,
            RbTeamPuzzleState::Unlocked | RbTeamPuzzleState::Solved
        )
    }
}

#[derive(FromRow, Serialize)]
pub struct RbTeamPuzzle {
    pub team_id: i32,
    pub puzzle_id: i32,
    pub pstate: RbTeamPuzzleState,
    pub ctime_at: OffsetDateTime,
}

#[derive(Serialize, Deserialize, FromPrimitive, IntoPrimitive, Clone, Copy, Eq, PartialEq)]
#[repr(i16)]
#[serde(into = "i16")]
pub enum RbJudgeAction {
    Error = -2,
    Pending = -1,
    Fail = 0,
    Correct = 1,
    Milestone = 2,
    StartGame = 3,
    EasterEgg = 4,

    #[num_enum(catch_all)]
    Invalid(i16),
}

impl From<&str> for RbJudgeAction {
    fn from(s: &str) -> Self {
        match s {
            "pending" => RbJudgeAction::Pending,
            "fail" => RbJudgeAction::Fail,
            "correct" => RbJudgeAction::Correct,
            "milestone" => RbJudgeAction::Milestone,
            _ => RbJudgeAction::Error,
        }
    }
}

impl From<String> for RbJudgeAction {
    fn from(s: String) -> Self {
        s.as_str().into()
    }
}

impl From<Option<String>> for RbJudgeAction {
    fn from(opt: Option<String>) -> Self {
        match opt {
            Some(s) => s.into(),
            None => RbJudgeAction::Error,
        }
    }
}

#[derive(FromRow, Serialize)]
pub struct RbSubmission {
    pub id: i32,
    pub team_id: i32,
    pub user_id: i32,
    pub puzzle_id: i32,
    pub user_answer: String,
    pub norm_answer: String,
    pub saction: RbJudgeAction,
    pub sresult: Option<String>,
    pub real_answer: Option<String>,
    pub ctime_at: OffsetDateTime,
}
