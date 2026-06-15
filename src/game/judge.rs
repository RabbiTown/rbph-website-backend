use std::future::Future;

use serde::{Deserialize, Serialize, de};
use serde_json::Value;

use crate::{error::RbInternalError, model::game::RbJudgeAction};

pub fn normalize_answer(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

#[derive(Debug, Deserialize)]
pub struct JudgeRule {
    #[serde(rename = "type")]
    pub rtype: Option<String>,
    text: Option<String>,
    action: Option<String>,
    result: Option<String>,
    answer: Option<String>,
    pub function: Option<String>,
}

#[derive(Serialize)]
pub struct JudgeResult {
    pub action: RbJudgeAction,
    pub result: Option<String>,
    pub answer: Option<String>,
    #[serde(default)]
    pub ignored: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct JudgeBackendOutput {
    #[serde(default, deserialize_with = "deserialize_judge_action")]
    pub action: Option<RbJudgeAction>,
    pub result: Option<String>,
    pub answer: Option<String>,
    #[serde(default)]
    pub ignored: Option<bool>,
}

impl From<JudgeBackendOutput> for JudgeResult {
    fn from(value: JudgeBackendOutput) -> Self {
        Self {
            action: value.action.unwrap_or(RbJudgeAction::Fail),
            result: value.result,
            answer: value.answer,
            ignored: value.ignored.unwrap_or(false),
        }
    }
}

fn deserialize_judge_action<'de, D>(deserializer: D) -> Result<Option<RbJudgeAction>, D::Error>
where
    D: de::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum RawAction {
        String(String),
        Number(i16),
        Null,
    }

    match Option::<RawAction>::deserialize(deserializer)? {
        Some(RawAction::String(value)) => Ok(Some(value.into())),
        Some(RawAction::Number(value)) => Ok(Some(value.into())),
        Some(RawAction::Null) | None => Ok(None),
    }
}

pub fn value_to_judge(v: Value) -> Result<Vec<JudgeRule>, RbInternalError> {
    let rules: Vec<JudgeRule> = serde_json::from_value(v)?;

    let rules = rules
        .into_iter()
        .map(|mut x| {
            if matches!(x.rtype.as_deref(), Some("exact")) {
                x.answer = x.answer.clone().or_else(|| x.text.clone());
                x.text = x.text.clone().map(|t| normalize_answer(&t));
            }
            x
        })
        .collect();

    Ok(rules)
}

pub async fn judge_by_rules<F, Fut>(
    rules: &[JudgeRule],
    answer: &str,
    mut custom_executor: F,
) -> Result<JudgeResult, RbInternalError>
where
    F: FnMut(&JudgeRule) -> Fut,
    Fut: Future<Output = Result<Option<JudgeBackendOutput>, RbInternalError>>,
{
    for rule in rules {
        match rule.rtype.as_deref() {
            Some("exact") => {
                if let Some(expected) = &rule.text
                    && expected == answer
                {
                    return Ok(JudgeResult {
                        action: rule.action.clone().into(),
                        result: rule.result.clone(),
                        answer: rule.answer.clone(),
                        ignored: false,
                    });
                }
            }
            Some("custom") => {
                if let Some(output) = custom_executor(rule).await? {
                    return Ok(output.into());
                }
            }
            Some("all") => {
                return Ok(JudgeResult {
                    action: rule.action.clone().into(),
                    result: rule.result.clone(),
                    answer: rule.answer.clone(),
                    ignored: false,
                });
            }
            _ => {}
        }
    }

    Ok(JudgeResult {
        action: RbJudgeAction::Fail,
        result: None,
        answer: None,
        ignored: false,
    })
}
