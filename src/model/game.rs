use num_enum::{FromPrimitive, IntoPrimitive};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use serde_repr::{Deserialize_repr, Serialize_repr};
use sqlx::{
    Decode, Encode, Postgres, Type,
    encode::IsNull,
    postgres::{PgArgumentBuffer, PgValueRef},
    prelude::FromRow,
    types::{Json, time::OffsetDateTime},
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
    pub settings: RbGameSettings,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub ctime_at: OffsetDateTime,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RbGameSettings {
    pub team: RbGameTeamSettings,
    pub ticket: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RbGameTeamSettings {
    pub max_members: i32,
}

pub trait GameSettingGroup: Default + for<'de> Deserialize<'de> + Serialize + Sized {
    const PATH: &'static [&'static str];

    fn sanitize(self) -> Self {
        self
    }
}

impl Default for RbGameSettings {
    fn default() -> Self {
        Self {
            team: RbGameTeamSettings::default(),
            ticket: Value::Object(Map::new()),
        }
    }
}

impl Default for RbGameTeamSettings {
    fn default() -> Self {
        Self { max_members: 6 }
    }
}

impl RbGameTeamSettings {
    pub fn validate_patch(value: &Value) -> bool {
        match value {
            Value::Null => true,
            Value::Object(team) => team.iter().all(|(key, value)| match key.as_str() {
                "max_members" => {
                    value.is_null()
                        || value.as_i64().is_some_and(|max_members| {
                            max_members > 0 && max_members <= i32::MAX as i64
                        })
                }
                _ => false,
            }),
            _ => false,
        }
    }
}

impl GameSettingGroup for RbGameTeamSettings {
    const PATH: &'static [&'static str] = &["team"];

    fn sanitize(mut self) -> Self {
        let default = Self::default();
        if self.max_members <= 0 {
            self.max_members = default.max_members;
        }
        self
    }
}

impl RbGameSettings {
    pub fn default_value() -> Value {
        serde_json::to_value(Self::default()).unwrap_or(Value::Object(Map::new()))
    }

    pub fn validate_patch(value: &Value) -> bool {
        let Value::Object(root) = value else {
            return false;
        };

        root.iter().all(|(key, value)| match key.as_str() {
            "team" => RbGameTeamSettings::validate_patch(value),
            "ticket" => value.is_null() || value.is_object(),
            _ => false,
        })
    }

    pub fn sanitize(value: Option<Value>) -> Self {
        let value = value.unwrap_or(Value::Null);
        let default = Self::default_value();
        let merged = Self::merge_patch(default, value);
        let mut settings = serde_json::from_value::<Self>(merged).unwrap_or_default();

        settings.team = settings.team.sanitize();
        if !settings.ticket.is_object() {
            settings.ticket = Self::default().ticket;
        }

        settings
    }

    pub fn sanitize_storage(value: Option<Value>) -> Value {
        let value = value.unwrap_or(Value::Object(Map::new()));
        if value.is_object() {
            value
        } else {
            Value::Object(Map::new())
        }
    }

    pub fn merge_patch(base: Value, patch: Value) -> Value {
        match (base, patch) {
            (Value::Object(mut base), Value::Object(patch)) => {
                for (key, value) in patch {
                    if value.is_null() {
                        base.remove(&key);
                    } else {
                        let base_value = base.remove(&key).unwrap_or(Value::Null);
                        base.insert(key, Self::merge_patch(base_value, value));
                    }
                }
                Value::Object(base)
            }
            (_, patch) => patch,
        }
    }
}

impl Type<Postgres> for RbGameSettings {
    fn type_info() -> <Postgres as sqlx::Database>::TypeInfo {
        <Json<Self> as Type<Postgres>>::type_info()
    }

    fn compatible(ty: &<Postgres as sqlx::Database>::TypeInfo) -> bool {
        <Json<Self> as Type<Postgres>>::compatible(ty)
    }
}

impl<'r> Decode<'r, Postgres> for RbGameSettings {
    fn decode(value: PgValueRef<'r>) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self::sanitize(Some(
            <Json<Value> as Decode<Postgres>>::decode(value)?.0,
        )))
    }
}

impl<'q> Encode<'q, Postgres> for RbGameSettings {
    fn encode_by_ref(
        &self,
        buf: &mut PgArgumentBuffer,
    ) -> Result<IsNull, Box<dyn std::error::Error + Send + Sync>> {
        <Json<&Self> as Encode<Postgres>>::encode_by_ref(&Json(self), buf)
    }
}

#[derive(Serialize_repr, Deserialize_repr, FromPrimitive, IntoPrimitive, Clone, Copy)]
#[repr(i16)]
#[serde(into = "i16")]
pub enum RbTeamState {
    Banned = -1,
    Open = 0,
    InGame = 1,
    Finished = 2,

    #[num_enum(default)]
    Invalid,
}

#[derive(FromRow, Serialize)]
pub struct RbTeam {
    pub id: i32,
    pub name: String,
    pub state: RbTeamState,
    pub pass: String,
    pub bio: String,
    pub game_id: i32,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub ctime_at: OffsetDateTime,
    #[serde(with = "crate::serde_helpers::serialize_option_offset_datetime")]
    pub finish_at: Option<OffsetDateTime>,
}

#[derive(Serialize_repr, Deserialize_repr, FromPrimitive, IntoPrimitive, Clone, Copy)]
#[repr(i16)]
#[serde(into = "i16")]
pub enum RbPuzzleType {
    Normal = 0,
    Story = 1,

    #[num_enum(default)]
    Invalid,
}

#[derive(Serialize_repr, Deserialize_repr, FromPrimitive, IntoPrimitive, Clone, Copy)]
#[repr(i16)]
#[serde(into = "i16")]
pub enum RbContentType {
    Markdown = 0,
    Html = 1,

    /// mark this content should be sanitized by frontend
    UnsafeMarkdown = 2,

    #[num_enum(default)]
    Invalid,
}

impl RbContentType {
    pub fn is_trusted(&self) -> bool {
        matches!(self, Self::Markdown | Self::Html)
    }
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

#[derive(Serialize_repr, Deserialize_repr, FromPrimitive, IntoPrimitive, Clone, Copy)]
#[repr(i16)]
#[serde(into = "i16")]
pub enum RbTeamPuzzleState {
    Locked = -1,
    Unlocked = 0,
    Solved = 1,

    #[num_enum(default)]
    Invalid,
}

impl RbTeamPuzzleState {
    pub fn accessible(&self) -> bool {
        matches!(
            self,
            RbTeamPuzzleState::Unlocked | RbTeamPuzzleState::Solved
        )
    }
}

#[derive(Serialize_repr, Deserialize_repr, FromPrimitive, IntoPrimitive, Clone, Copy)]
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

    #[num_enum(default)]
    Invalid,
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

#[derive(Serialize_repr, Deserialize_repr, FromPrimitive, IntoPrimitive, Clone, Copy)]
#[repr(i16)]
#[serde(into = "i16")]
pub enum RbTicketState {
    Closed = 0,
    Open = 1,

    #[num_enum(default)]
    Invalid,
}

#[derive(Serialize_repr, Deserialize_repr, FromPrimitive, IntoPrimitive, Clone, Copy)]
#[repr(i16)]
#[serde(into = "i16")]
pub enum RbTicketSenderType {
    Team = 0,
    Host = 1,

    #[num_enum(default)]
    Invalid,
}
