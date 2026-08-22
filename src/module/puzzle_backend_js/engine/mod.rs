mod boa;

use std::sync::Arc;

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
}

pub(super) trait JsEngine: Send + Sync {
    fn execute(
        &self,
        request: EngineRequest,
        host: Arc<dyn HostBridge>,
    ) -> Result<HostValue, RbInternalError>;
}

static BOA_ENGINE: boa::BoaEngine = boa::BoaEngine;

pub(super) fn active_engine() -> &'static dyn JsEngine {
    &BOA_ENGINE
}

pub(super) fn internal_err(message: impl Into<String>) -> RbInternalError {
    RbInternalError::Other(message.into())
}
