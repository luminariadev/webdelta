use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::config::ConditionConfig;
use crate::error::{Error, Result};
use crate::numeric::NumericDelta;

/// Outcome of evaluating configured watch conditions against a content change.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuleEvaluation {
    pub triggered: bool,
    pub reasons: Vec<String>,
}

/// Evaluates all `conditions` using logical AND semantics.
///
/// When `conditions` is empty, defaults to `[ConditionConfig::Changed]`.
pub fn evaluate_conditions(
    conditions: &[ConditionConfig],
    previous: &str,
    current: &str,
    numeric_delta: Option<&NumericDelta>,
) -> Result<RuleEvaluation> {
    let effective = if conditions.is_empty() {
        vec![ConditionConfig::Changed]
    } else {
        conditions.to_vec()
    };

    let mut reasons = Vec::new();
    for condition in &effective {
        match evaluate_single(condition, previous, current, numeric_delta)? {
            Some(reason) => reasons.push(reason),
            None => {
                return Ok(RuleEvaluation {
                    triggered: false,
                    reasons: Vec::new(),
                });
            }
        }
    }

    Ok(RuleEvaluation {
        triggered: true,
        reasons,
    })
}

fn evaluate_single(
    condition: &ConditionConfig,
    previous: &str,
    current: &str,
    numeric_delta: Option<&NumericDelta>,
) -> Result<Option<String>> {
    match condition {
        ConditionConfig::Changed => {
            if previous != current {
                Ok(Some("content changed".to_string()))
            } else {
                Ok(None)
            }
        }
        ConditionConfig::Contains {
            value,
            case_sensitive,
        } => {
            let matched = if *case_sensitive {
                current.contains(value)
            } else {
                current.to_lowercase().contains(&value.to_lowercase())
            };
            Ok(matched.then(|| format!("contains '{value}'")))
        }
        ConditionConfig::NotContains {
            value,
            case_sensitive,
        } => {
            let matched = if *case_sensitive {
                !current.contains(value)
            } else {
                !current.to_lowercase().contains(&value.to_lowercase())
            };
            Ok(matched.then(|| format!("does not contain '{value}'")))
        }
        ConditionConfig::RegexMatch { pattern } => {
            let re = Regex::new(pattern)
                .map_err(|err| Error::Rule(format!("invalid regex_match '{pattern}': {err}")))?;
            Ok(re
                .is_match(current)
                .then(|| format!("matched regex '{pattern}'")))
        }
        ConditionConfig::NumericLt { threshold } => Ok(numeric_delta.and_then(|d| {
            (d.current < *threshold).then(|| format!("value {} < {threshold}", d.current))
        })),
        ConditionConfig::NumericLte { threshold } => Ok(numeric_delta.and_then(|d| {
            (d.current <= *threshold).then(|| format!("value {} <= {threshold}", d.current))
        })),
        ConditionConfig::NumericGt { threshold } => Ok(numeric_delta.and_then(|d| {
            (d.current > *threshold).then(|| format!("value {} > {threshold}", d.current))
        })),
        ConditionConfig::NumericGte { threshold } => Ok(numeric_delta.and_then(|d| {
            (d.current >= *threshold).then(|| format!("value {} >= {threshold}", d.current))
        })),
        ConditionConfig::DropPctAtLeast { percent } => Ok(numeric_delta.and_then(|d| {
            d.percent_change.and_then(|pct| {
                (pct <= -*percent)
                    .then(|| format!("dropped by {:.2}% (threshold {percent:.2}%)", -pct))
            })
        })),
        ConditionConfig::RisePctAtLeast { percent } => Ok(numeric_delta.and_then(|d| {
            d.percent_change.and_then(|pct| {
                (pct >= *percent).then(|| format!("rose by {:.2}% (threshold {percent:.2}%)", pct))
            })
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::numeric::compute_delta;

    #[test]
    fn test_default_changed_and_and_semantics() {
        let eval = evaluate_conditions(&[], "a", "b", None).unwrap();
        assert!(eval.triggered);

        let delta = compute_delta(100.0, 80.0);
        let conds = vec![
            ConditionConfig::DropPctAtLeast { percent: 15.0 },
            ConditionConfig::NumericLt { threshold: 90.0 },
        ];
        let matched = evaluate_conditions(&conds, "$100", "$80", Some(&delta)).unwrap();
        assert!(matched.triggered);
        assert_eq!(matched.reasons.len(), 2);

        let not_matched = evaluate_conditions(
            &[ConditionConfig::DropPctAtLeast { percent: 25.0 }],
            "$100",
            "$80",
            Some(&delta),
        )
        .unwrap();
        assert!(!not_matched.triggered);
    }
}
