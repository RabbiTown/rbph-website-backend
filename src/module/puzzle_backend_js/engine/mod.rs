#[cfg(any(not(feature = "v8-engine"), test))]
mod boa;
#[cfg(test)]
mod tests;
#[cfg(feature = "v8-engine")]
mod v8;

use std::{sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::RbInternalError;

use super::{host::HostBridge, protocol::HostValue};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum ExecutionKind {
    Api,
    Judge,
    HintPurchase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum ResultMode {
    JsonRequired,
    UndefinedAllowed,
    Ignored,
}

pub(super) struct EngineRequest {
    pub source: String,
    pub function_name: String,
    pub execution_kind: ExecutionKind,
    pub argument: Value,
    pub bootstrap_metadata: Value,
    pub result_mode: ResultMode,
    pub wall_time_limit: Duration,
}

pub(super) trait JsEngine: Send + Sync {
    fn execute(
        &self,
        request: EngineRequest,
        host: Arc<dyn HostBridge>,
    ) -> Result<HostValue, RbInternalError>;
}

#[cfg(not(feature = "v8-engine"))]
static BOA_ENGINE: boa::BoaEngine = boa::BoaEngine;
#[cfg(feature = "v8-engine")]
static V8_ENGINE: v8::V8Engine = v8::V8Engine;

pub(super) fn active_engine() -> &'static dyn JsEngine {
    #[cfg(feature = "v8-engine")]
    {
        &V8_ENGINE
    }
    #[cfg(not(feature = "v8-engine"))]
    {
        &BOA_ENGINE
    }
}

pub(super) fn internal_err(message: impl Into<String>) -> RbInternalError {
    RbInternalError::Other(message.into())
}
