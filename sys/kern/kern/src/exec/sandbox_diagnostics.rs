//! Diagnostic capture never changes the sandbox policy or triggers a retry.

#[cfg(target_os = "macos")]
use super::seatbelt::{DenialLogger, SandboxDenial};
use super::{ExecToolCallOutput, SandboxType};
#[cfg(target_os = "macos")]
use tokio::sync::{Mutex, OnceCell};

#[derive(Debug)]
pub(super) struct SandboxDiagnostics {
    #[cfg(target_os = "macos")]
    enabled: bool,
    #[cfg(target_os = "macos")]
    logger: Mutex<Option<DenialLogger>>,
    #[cfg(target_os = "macos")]
    report: OnceCell<String>,
}

impl SandboxDiagnostics {
    pub(super) fn start(sandbox: SandboxType) -> Self {
        #[cfg(target_os = "macos")]
        {
            let enabled = sandbox == SandboxType::MacosSeatbelt;
            Self {
                enabled,
                logger: Mutex::new(enabled.then(DenialLogger::new).flatten()),
                report: OnceCell::new(),
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = sandbox;
            Self {}
        }
    }

    pub(super) fn on_spawn(&mut self, pid: Option<u32>) {
        #[cfg(target_os = "macos")]
        if let Some(pid) = pid
            && let Some(logger) = self.logger.get_mut().as_mut()
        {
            logger.track_pid(pid);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = pid;
    }

    pub(super) async fn finish(&self) {
        #[cfg(target_os = "macos")]
        if self.enabled {
            self.report
                .get_or_init(|| async {
                    let logger = self.logger.lock().await.take();
                    let denials = match logger {
                        Some(logger) => logger.finish().await,
                        None => Vec::new(),
                    };
                    format_report(&denials)
                })
                .await;
        }
    }

    pub(super) async fn enrich_denial(&self, output: &mut ExecToolCallOutput) {
        self.finish().await;
        #[cfg(target_os = "macos")]
        if let Some(report) = self.report.get() {
            prepend_report(report, output);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = output;
    }
}

#[cfg(target_os = "macos")]
fn format_report(denials: &[SandboxDenial]) -> String {
    if denials.is_empty() {
        return "macOS sandbox diagnostics unavailable: no matching audit event was captured. \
                EPERM alone does not identify the denied operation or path.\n"
            .to_string();
    }
    let mut report = "macOS sandbox denials (command process tree):\n".to_string();
    for denial in denials {
        report.push_str(&format!(
            "  ({}) {}\n",
            denial.name.escape_debug(),
            denial.capability.escape_debug()
        ));
    }
    const MAX_REPORT_BYTES: usize = 8 * 1024;
    const TRUNCATED: &str = "\n[additional sandbox diagnostics truncated]\n";
    if report.len() > MAX_REPORT_BYTES {
        report.truncate(report.floor_char_boundary(MAX_REPORT_BYTES - TRUNCATED.len()));
        report.push_str(TRUNCATED);
    }
    report
}

#[cfg(target_os = "macos")]
fn prepend_report(report: &str, output: &mut ExecToolCallOutput) {
    // Put evidence first so output/UI truncation does not hide the denied path.
    output.stderr.text.insert_str(0, report);
    output.aggregated_output.text.insert_str(0, report);
}

#[cfg(all(test, target_os = "macos"))]
mod tests;
