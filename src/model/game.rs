use num_enum::{FromPrimitive, IntoPrimitive};
use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};
use sqlx::{
    Decode, Postgres, Type, postgres::PgValueRef, prelude::FromRow, types::time::OffsetDateTime,
};

#[derive(FromRow, Serialize)]
pub struct RbGame {
    pub id: i32,
    pub title: String,
    pub cover: Option<String>,
    pub is_shown: bool,
    pub is_online: bool,
    #[serde(with = "crate::serde_helpers::serialize_option_offset_datetime")]
    pub reg_open_at: Option<OffsetDateTime>,
    #[serde(with = "crate::serde_helpers::serialize_option_offset_datetime")]
    pub pre_open_at: Option<OffsetDateTime>,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub start_at: OffsetDateTime,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub end_at: OffsetDateTime,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub ctime_at: OffsetDateTime,
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
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub ctime_at: OffsetDateTime,
    #[serde(with = "crate::serde_helpers::serialize_option_offset_datetime")]
    pub finish_at: Option<OffsetDateTime>,
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

impl Type<sqlx::Postgres> for RbContentType {
    fn type_info() -> <sqlx::Postgres as sqlx::Database>::TypeInfo {
        <i16 as Type<sqlx::Postgres>>::type_info()
    }
}

impl<'r> Decode<'r, Postgres> for RbContentType {
    fn decode(value: PgValueRef<'r>) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(<i16 as Decode<Postgres>>::decode(value)?.into())
    }
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
    FinishGame = 5,

    #[num_enum(catch_all)]
    Invalid(i16),
}

impl RbJudgeAction {
    pub fn side_effect(&self) -> bool {
        matches!(
            self,
            RbJudgeAction::Correct | RbJudgeAction::StartGame | RbJudgeAction::FinishGame
        )
    }
}

impl From<&str> for RbJudgeAction {
    fn from(s: &str) -> Self {
        match s {
            "pending" => RbJudgeAction::Pending,
            "fail" => RbJudgeAction::Fail,
            "correct" => RbJudgeAction::Correct,
            "milestone" => RbJudgeAction::Milestone,
            "start_game" => RbJudgeAction::StartGame,
            "easter_egg" => RbJudgeAction::EasterEgg,
            "finish_game" => RbJudgeAction::FinishGame,
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

#[derive(Serialize_repr, Deserialize_repr, Clone, Copy, Eq, PartialEq)]
#[repr(i16)]
#[serde(into = "i16")]
pub enum RbPuzzlePenaltyType {
    No = 0,
    FixedTime = 1,
    LinearTime = 2,
    Currency = 3,
}
