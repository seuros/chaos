use std::future::Future;
use std::pin::Pin;

use tracing::debug;

use crate::error::ReflexError;
use crate::judgment::Judgment;
use crate::judgment::JudgmentKind;
use crate::judgment::Verdict;

pub type ReflexFuture<'a> = Pin<Box<dyn Future<Output = Result<Verdict, ReflexError>> + Send + 'a>>;

/// One model that can answer some judgments.
pub trait ReflexBackend: Send + Sync + std::fmt::Debug {
    fn name(&self) -> &str;
    fn supports(&self, kind: JudgmentKind) -> bool;
    fn judge(&self, judgment: Judgment) -> ReflexFuture<'_>;
}

/// Ordered set of backends. The first backend supporting a judgment answers it.
#[derive(Debug, Default)]
pub struct Reflex {
    backends: Vec<Box<dyn ReflexBackend>>,
}

impl Reflex {
    pub fn new(backends: Vec<Box<dyn ReflexBackend>>) -> Self {
        Self { backends }
    }

    pub fn is_empty(&self) -> bool {
        self.backends.is_empty()
    }

    pub fn supports(&self, kind: JudgmentKind) -> bool {
        self.backend_for(kind).is_some()
    }

    pub fn backend_for(&self, kind: JudgmentKind) -> Option<&dyn ReflexBackend> {
        self.backends
            .iter()
            .map(AsRef::as_ref)
            .find(|backend| backend.supports(kind))
    }

    pub async fn judge(&self, judgment: Judgment) -> Result<Verdict, ReflexError> {
        judgment.validate()?;
        let kind = judgment.kind();
        let backend = self
            .backend_for(kind)
            .ok_or(ReflexError::Unsupported(kind))?;
        debug!(
            backend = backend.name(),
            ?kind,
            "reflex judgment dispatched"
        );
        backend.judge(judgment).await?.validate()
    }
}
