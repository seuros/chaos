use std::collections::BTreeMap;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use super::error::JevError;

/// Upper bound on options in a Choice question.
pub const MAX_CHOICE_OPTIONS: usize = 255;
/// Lower bound on levels in a Score question.
pub const MIN_SCORE_LEVELS: usize = 2;
/// Upper bound on levels in a Score question.
pub const MAX_SCORE_LEVELS: usize = 10;

/// Questions keyed by caller-chosen ids. Answers come back under the same ids.
pub type Questions = BTreeMap<String, Question>;

/// One typed question evaluated against the request state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    /// Yes/no question. The answer is the probability of "yes".
    Noul {
        instructions: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        criteria: Option<NoulCriteria>,
    },
    /// Pick one option. Keys are option names, values are optional descriptions.
    Choice {
        instructions: String,
        criteria: BTreeMap<String, Option<String>>,
    },
    /// Rate on ordered levels, lowest first.
    Score {
        instructions: String,
        criteria: Vec<String>,
    },
}

/// Optional descriptions of the "yes" and "no" outcomes of a Noul question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoulCriteria {
    #[serde(rename = "true")]
    pub yes: String,
    #[serde(rename = "false")]
    pub no: String,
}

impl Question {
    pub fn noul(instructions: impl Into<String>) -> Self {
        Self::Noul {
            instructions: instructions.into(),
            criteria: None,
        }
    }

    pub fn noul_with_criteria(
        instructions: impl Into<String>,
        yes: impl Into<String>,
        no: impl Into<String>,
    ) -> Self {
        Self::Noul {
            instructions: instructions.into(),
            criteria: Some(NoulCriteria {
                yes: yes.into(),
                no: no.into(),
            }),
        }
    }

    pub fn choice<K, V>(
        instructions: impl Into<String>,
        options: impl IntoIterator<Item = (K, Option<V>)>,
    ) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        Self::Choice {
            instructions: instructions.into(),
            criteria: options
                .into_iter()
                .map(|(key, description)| (key.into(), description.map(Into::into)))
                .collect(),
        }
    }

    pub fn score<L: Into<String>>(
        instructions: impl Into<String>,
        levels: impl IntoIterator<Item = L>,
    ) -> Self {
        Self::Score {
            instructions: instructions.into(),
            criteria: levels.into_iter().map(Into::into).collect(),
        }
    }

    pub fn validate(&self, key: &str) -> Result<(), JevError> {
        let invalid = |reason: &str| JevError::InvalidQuestion {
            key: key.to_string(),
            reason: reason.to_string(),
        };
        let instructions = match self {
            Self::Noul { instructions, .. }
            | Self::Choice { instructions, .. }
            | Self::Score { instructions, .. } => instructions,
        };
        if instructions.trim().is_empty() {
            return Err(invalid("instructions are empty"));
        }
        match self {
            Self::Noul { .. } => Ok(()),
            Self::Choice { criteria, .. } => {
                if criteria.is_empty() {
                    return Err(invalid("choice needs at least one option"));
                }
                if criteria.len() > MAX_CHOICE_OPTIONS {
                    return Err(invalid("choice accepts at most 255 options"));
                }
                if criteria.keys().any(|option| option.trim().is_empty()) {
                    return Err(invalid("choice option names must not be empty"));
                }
                Ok(())
            }
            Self::Score { criteria, .. } => {
                if criteria.len() < MIN_SCORE_LEVELS {
                    return Err(invalid("score needs at least two levels"));
                }
                if criteria.len() > MAX_SCORE_LEVELS {
                    return Err(invalid("score accepts at most ten levels"));
                }
                if criteria.iter().any(|level| level.trim().is_empty()) {
                    return Err(invalid("score levels must not be empty"));
                }
                Ok(())
            }
        }
    }
}

/// Wire body for `POST /v1/systemone`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemOneRequest {
    pub state: Value,
    pub model: String,
    pub questions: Questions,
}

/// Wire body returned by `POST /v1/systemone`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemOneResponse {
    pub model: String,
    pub answers: BTreeMap<String, Answer>,
    #[serde(default)]
    pub usage: Usage,
}

impl SystemOneResponse {
    /// Validate answer types, completeness, and ranges against the request.
    pub(crate) fn validate(&self, questions: &Questions) -> Result<(), JevError> {
        if self.model.trim().is_empty() {
            return Err(JevError::Decode("response model is empty".into()));
        }
        for (key, question) in questions {
            let valid = match (question, self.answer(key)?) {
                (Question::Noul { .. }, Answer::Noul { noul }) => is_probability(*noul),
                (
                    Question::Choice { criteria, .. },
                    Answer::Choice {
                        choice,
                        confidence,
                        probabilities,
                    },
                ) => {
                    criteria.contains_key(choice)
                        && is_probability(*confidence)
                        && valid_distribution(probabilities, criteria.keys())
                }
                (
                    Question::Score { criteria, .. },
                    Answer::Score {
                        score,
                        confidence,
                        probabilities,
                        ..
                    },
                ) => {
                    (0.0..=criteria.len().saturating_sub(1) as f64).contains(score)
                        && is_probability(*confidence)
                        && valid_distribution(
                            probabilities,
                            (0..criteria.len()).map(|index| index.to_string()),
                        )
                }
                _ => false,
            };
            if !valid {
                return Err(JevError::Decode(format!(
                    "answer `{key}` does not match its question or contains invalid values"
                )));
            }
        }
        Ok(())
    }

    pub fn answer(&self, key: &str) -> Result<&Answer, JevError> {
        self.answers
            .get(key)
            .ok_or_else(|| JevError::MissingAnswer(key.to_string()))
    }

    pub fn noul(&self, key: &str) -> Result<f64, JevError> {
        match self.answer(key)? {
            Answer::Noul { noul } => Ok(*noul),
            Answer::Choice { .. } | Answer::Score { .. } => {
                Err(JevError::Decode(format!("answer `{key}` is not a noul")))
            }
        }
    }

    pub fn choice(&self, key: &str) -> Result<(&str, f64), JevError> {
        match self.answer(key)? {
            Answer::Choice {
                choice, confidence, ..
            } => Ok((choice.as_str(), *confidence)),
            Answer::Noul { .. } | Answer::Score { .. } => {
                Err(JevError::Decode(format!("answer `{key}` is not a choice")))
            }
        }
    }

    pub fn score(&self, key: &str) -> Result<(f64, f64), JevError> {
        match self.answer(key)? {
            Answer::Score {
                score, confidence, ..
            } => Ok((*score, *confidence)),
            Answer::Noul { .. } | Answer::Choice { .. } => {
                Err(JevError::Decode(format!("answer `{key}` is not a score")))
            }
        }
    }
}

fn is_probability(value: f64) -> bool {
    (0.0..=1.0).contains(&value)
}

fn valid_distribution<K: AsRef<str>>(
    probabilities: &BTreeMap<String, f64>,
    mut expected: impl ExactSizeIterator<Item = K>,
) -> bool {
    probabilities.len() == expected.len()
        && expected.all(|key| {
            probabilities
                .get(key.as_ref())
                .is_some_and(|value| is_probability(*value))
        })
}

/// One answer, shaped by the question type it replies to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Answer {
    Noul {
        noul: f64,
    },
    Choice {
        choice: String,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
    Score {
        score: f64,
        #[serde(default)]
        legend: BTreeMap<String, String>,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
}

impl Answer {
    /// Probability mass the model placed on `key`, for Choice and Score answers.
    pub fn probability_of(&self, key: &str) -> Option<f64> {
        match self {
            Self::Noul { .. } => None,
            Self::Choice { probabilities, .. } | Self::Score { probabilities, .. } => {
                probabilities.get(key).copied()
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Wire body returned by `GET /v1/models`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelsResponse {
    pub models: Vec<ModelCard>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCard {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub release_date: String,
}
