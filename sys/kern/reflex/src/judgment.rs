use std::collections::BTreeMap;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use crate::error::ReflexError;

/// A typed judgment request.
#[derive(Debug, Clone, PartialEq)]
pub enum Judgment {
    /// Is `claim` supported by `document`?
    Grounding { document: String, claim: String },
    /// Does the prompt or response violate the caller's nonblank policy?
    PolicyViolation {
        prompt: String,
        response: Option<String>,
        policy: String,
    },
    /// How much harm could `action` cause given `conversation`?
    ActionRisk {
        conversation: Value,
        action: Value,
        instructions: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JudgmentKind {
    Grounding,
    PolicyViolation,
    ActionRisk,
}

impl Judgment {
    pub(crate) fn validate(&self) -> Result<(), ReflexError> {
        match self {
            Self::PolicyViolation { policy, .. } if policy.trim().is_empty() => {
                Err(ReflexError::MissingPolicy)
            }
            _ => Ok(()),
        }
    }

    pub fn kind(&self) -> JudgmentKind {
        match self {
            Self::Grounding { .. } => JudgmentKind::Grounding,
            Self::PolicyViolation { .. } => JudgmentKind::PolicyViolation,
            Self::ActionRisk { .. } => JudgmentKind::ActionRisk,
        }
    }
}

/// Signal names reported alongside an action-risk verdict.
pub struct ActionRiskSignals;

impl ActionRiskSignals {
    /// Probability that the action destroys or irreversibly changes state.
    pub const IRREVERSIBLE: &'static str = "irreversible";
    /// Probability that the action exceeds what the user asked for.
    pub const BEYOND_REQUEST: &'static str = "beyond_request";
    /// Probability that the action sends private data to an external party.
    pub const EXFILTRATES: &'static str = "exfiltrates";
}

/// Headline signal plus any secondary probabilities a backend can offer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    /// P(yes), except Jev action risk: normalized harm score, not calibrated probability.
    pub probability: f64,
    /// Distribution confidence in [0, 1]; binary backends report 1.
    pub confidence: f64,
    /// Named secondary probabilities, empty for single-signal backends.
    #[serde(default)]
    pub signals: BTreeMap<String, f64>,
    /// Name of the backend that answered.
    pub backend: String,
}

impl Verdict {
    pub(crate) fn binary(
        backend: impl Into<String>,
        probability: f64,
    ) -> Result<Self, ReflexError> {
        Self {
            probability,
            confidence: 1.0,
            signals: BTreeMap::new(),
            backend: backend.into(),
        }
        .validate()
    }

    pub(crate) fn validate(self) -> Result<Self, ReflexError> {
        for (name, value) in [
            ("probability", self.probability),
            ("confidence", self.confidence),
        ]
        .into_iter()
        .chain(
            self.signals
                .iter()
                .map(|(name, value)| (name.as_str(), *value)),
        ) {
            if !(0.0..=1.0).contains(&value) {
                return Err(ReflexError::malformed(
                    &self.backend,
                    format!("{name} must be a finite probability in [0, 1], got {value}"),
                ));
            }
        }
        Ok(self)
    }

    pub fn signal(&self, name: &str) -> Option<f64> {
        self.signals.get(name).copied()
    }
}
