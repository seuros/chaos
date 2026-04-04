//! Standard type to use with the `--approval-mode` CLI option.

use clap::ValueEnum;

use chaos_ipc::protocol::ApprovalPolicy;

#[derive(Clone, Copy, Debug, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ApprovalModeCliArg {
    /// Only run "trusted" commands (e.g. ls, cat, sed) without asking for user
    /// approval. Will escalate to the user if the model proposes a command that
    /// is not in the "trusted" set.
    Supervised,

    /// The model decides when to ask the user for approval.
    Interactive,

    /// Never ask for user approval.
    /// Execution failures are immediately returned to the model.
    Headless,
}

impl From<ApprovalModeCliArg> for ApprovalPolicy {
    fn from(value: ApprovalModeCliArg) -> Self {
        match value {
            ApprovalModeCliArg::Supervised => ApprovalPolicy::Supervised,
            ApprovalModeCliArg::Interactive => ApprovalPolicy::Interactive,
            ApprovalModeCliArg::Headless => ApprovalPolicy::Headless,
        }
    }
}
