use serde::{Deserialize, Serialize};

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
    rtype: Option<String>,
    text: Option<String>,
    action: Option<String>,
    result: Option<String>,
    answer: Option<String>,
}

#[derive(Serialize)]
pub struct JudgeResult {
    pub action: RbJudgeAction,
    pub result: Option<String>,
    pub answer: Option<String>,
}

pub fn parse_judge(s: &str) -> Result<Vec<JudgeRule>, RbInternalError> {
    let rules: Vec<JudgeRule> = serde_json::from_str(s)?;

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

pub fn judge_by_json(s: &str, answer: &str) -> Result<JudgeResult, RbInternalError> {
    judge_by_rules(&parse_judge(s)?, answer)
}

pub fn judge_by_rules(rules: &[JudgeRule], answer: &str) -> Result<JudgeResult, RbInternalError> {
    for rule in rules {
        match rule.rtype.as_deref() {
            Some("exact") => {
                if let Some(expected) = &rule.text {
                    if expected == answer {
                        return Ok(JudgeResult {
                            action: rule.action.clone().into(),
                            result: rule.result.clone(),
                            answer: rule.answer.clone(),
                        });
                    }
                }
            }
            Some("all") => {
                return Ok(JudgeResult {
                    action: rule.action.clone().into(),
                    result: rule.result.clone(),
                    answer: rule.answer.clone(),
                });
            }
            _ => {}
        }
    }

    Ok(JudgeResult {
        action: RbJudgeAction::Fail,
        result: None,
        answer: None,
    })
}
