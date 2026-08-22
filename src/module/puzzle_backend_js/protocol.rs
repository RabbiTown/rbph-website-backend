use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fmt;

pub(super) const HOST_PROTOCOL_VERSION: u16 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(super) enum HostScope {
    Global,
    Team { team_id: i32 },
    Puzzle { puzzle_id: i32 },
    TeamPuzzle { team_id: i32, puzzle_id: i32 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub(super) enum HostCurrencyRef {
    Id(i32),
    Slug(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "mode",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(super) enum HostKvExpiry {
    Preserve,
    Permanent,
    Ttl { ttl_ms: i64 },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct HostStoreSchema {
    #[serde(default)]
    pub indexes: Map<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct HostStoreListOptions {
    #[serde(default, rename = "where")]
    pub where_: Map<String, Value>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub cursor: Option<Value>,
    #[serde(default)]
    pub order: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct HostCurrencyUpdate {
    #[serde(default)]
    pub amount: Option<String>,
    #[serde(default)]
    pub team_growth: Option<String>,
    #[serde(default)]
    pub hidden: Option<bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum HostConsoleLevel {
    Debug,
    Log,
    Info,
    Warn,
    Error,
}

impl HostConsoleLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Log => "log",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "operation",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(super) enum HostCall {
    KvGet {
        scope: HostScope,
        key: String,
    },
    KvGetEntry {
        scope: HostScope,
        key: String,
    },
    KvSet {
        scope: HostScope,
        key: String,
        value: Value,
        expiry: HostKvExpiry,
    },
    KvIncrement {
        scope: HostScope,
        key: String,
        amount: f64,
        expiry: HostKvExpiry,
    },
    KvSetIfAbsent {
        scope: HostScope,
        key: String,
        value: Value,
        expiry: HostKvExpiry,
    },
    KvCompareAndSet {
        scope: HostScope,
        key: String,
        expected_version: String,
        value: Value,
        expiry: HostKvExpiry,
    },
    KvDelete {
        scope: HostScope,
        key: String,
    },
    StoreInsert {
        scope: HostScope,
        collection: String,
        schema: HostStoreSchema,
        value: Value,
    },
    StoreGet {
        scope: HostScope,
        collection: String,
        doc_id: String,
    },
    StoreList {
        scope: HostScope,
        collection: String,
        schema: HostStoreSchema,
        options: HostStoreListOptions,
    },
    CurrencyQuery {
        team_id: i32,
        check_team: bool,
        currency: Option<HostCurrencyRef>,
    },
    CurrencyCost {
        team_id: i32,
        check_team: bool,
        currency: HostCurrencyRef,
        amount: String,
        reason: Option<String>,
    },
    CurrencyAdd {
        team_id: i32,
        check_team: bool,
        currency: HostCurrencyRef,
        amount: String,
        reason: Option<String>,
    },
    CurrencyUpdate {
        team_id: i32,
        check_team: bool,
        currency: HostCurrencyRef,
        options: HostCurrencyUpdate,
        reason: Option<String>,
    },
    AssetList {
        object_key: String,
    },
    AssetReadText {
        object_key: String,
        relative_path: String,
    },
    AssetReadJson {
        object_key: String,
        relative_path: String,
    },
    AssetReadBytes {
        object_key: String,
        relative_path: String,
    },
    SubmissionAdd {
        submission: Value,
    },
    PuzzleSolve {
        submission: Value,
    },
    EventEmit {
        event: String,
        payload: Value,
    },
    ConsoleWrite {
        level: HostConsoleLevel,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct HostRequest {
    pub protocol_version: u16,
    pub call: HostCall,
}

impl HostRequest {
    pub fn current(call: HostCall) -> Self {
        Self {
            protocol_version: HOST_PROTOCOL_VERSION,
            call,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub(super) enum HostValue {
    Json(Value),
    Undefined,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum HostErrorKind {
    InvalidArgument,
    Forbidden,
    NotFound,
    LimitExceeded,
    Timeout,
    Unavailable,
    Internal,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct HostError {
    pub kind: HostErrorKind,
    pub message: String,
}

impl HostError {
    pub fn new(kind: HostErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(HostErrorKind::InvalidArgument, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(HostErrorKind::Internal, message)
    }
}

impl fmt::Display for HostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for HostError {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn host_calls_round_trip_through_json() {
        let calls = vec![
            HostCall::KvSet {
                scope: HostScope::TeamPuzzle {
                    team_id: 2,
                    puzzle_id: 3,
                },
                key: "answer".to_string(),
                value: json!({ "ready": true }),
                expiry: HostKvExpiry::Ttl { ttl_ms: 30_000 },
            },
            HostCall::StoreList {
                scope: HostScope::Global,
                collection: "scores".to_string(),
                schema: HostStoreSchema {
                    indexes: Map::from_iter([("score".to_string(), json!("number"))]),
                },
                options: HostStoreListOptions {
                    where_: Map::from_iter([("score".to_string(), json!({ "eq": 10 }))]),
                    limit: Some(20),
                    cursor: Some(json!("9")),
                    order: Some("asc".to_string()),
                },
            },
            HostCall::CurrencyUpdate {
                team_id: 4,
                check_team: true,
                currency: HostCurrencyRef::Slug("coin".to_string()),
                options: HostCurrencyUpdate {
                    amount: Some("5".to_string()),
                    team_growth: Some("2".to_string()),
                    hidden: Some(false),
                },
                reason: Some("bonus".to_string()),
            },
            HostCall::AssetReadBytes {
                object_key: "guide".to_string(),
                relative_path: "data.bin".to_string(),
            },
            HostCall::ConsoleWrite {
                level: HostConsoleLevel::Warn,
                message: "careful".to_string(),
            },
        ];

        for call in calls {
            let request = HostRequest::current(call);
            let json = serde_json::to_value(&request).expect("host request should serialize");
            assert_eq!(json["protocolVersion"], HOST_PROTOCOL_VERSION);
            assert_eq!(
                serde_json::from_value::<HostRequest>(json)
                    .expect("host request should deserialize"),
                request
            );
        }
    }

    #[test]
    fn host_value_and_error_round_trip_through_json() {
        let values = [HostValue::Undefined, HostValue::Json(json!([1, 2, 3]))];
        for value in values {
            let json = serde_json::to_value(&value).expect("host value should serialize");
            assert_eq!(
                serde_json::from_value::<HostValue>(json).expect("host value should deserialize"),
                value
            );
        }

        let error = HostError::new(HostErrorKind::Forbidden, "not available");
        let json = serde_json::to_value(&error).expect("host error should serialize");
        assert_eq!(
            serde_json::from_value::<HostError>(json).expect("host error should deserialize"),
            error
        );
    }
}
