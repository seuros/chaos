//! Diagnostics must not turn a disconnected terminal into a panic or double panic.

use std::fmt;
use std::io::{self, Write};

pub(crate) fn write_stderr(message: fmt::Arguments<'_>) {
    // In particular, FreeBSD returns ENXIO after a terminal has been revoked.
    // Diagnostics are best effort; eprintln! would panic on this I/O error.
    let _ = writeln!(io::stderr().lock(), "{message}");
}

pub(crate) fn install_panic_hook() -> color_eyre::Result<()> {
    let (panic_hook, eyre_hook) = color_eyre::config::HookBuilder::default().try_into_hooks()?;
    eyre_hook.install()?;
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("panic: {info}");
        // Keep color-eyre's rich report, not its eprintln!-based output hook.
        // libui chains terminal restoration ahead of this hook.
        write_stderr(format_args!("{}", panic_hook.panic_report(info)));
    }));
    Ok(())
}

#[cfg(test)]
#[path = "terminal_output/tests.rs"]
mod tests;
