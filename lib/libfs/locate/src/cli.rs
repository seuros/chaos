use std::num::NonZero;
use std::path::PathBuf;

/// Fuzzy matches filenames under a directory.
#[derive(usage::Cli)]
#[usage(
    bin = "chaos-locate",
    version,
    unknown_flags = "error",
    args_override_self = false
)]
pub struct Cli {
    /// Whether to output results in JSON format.
    #[usage(long)]
    pub json: bool,

    /// Maximum number of results to return.
    #[usage(long, short = 'l', default = "64")]
    pub limit: NonZero<usize>,

    /// Directory to search.
    #[usage(long, short = 'C')]
    pub cwd: Option<PathBuf>,

    /// Include matching file indices in the output.
    #[usage(long)]
    pub compute_indices: bool,

    // While it is common to default to the number of logical CPUs when creating
    // a thread pool, empirically, the I/O of the filetree traversal offers
    // limited parallelism and is the bottleneck, so using a smaller number of
    // threads is more efficient. (Empirically, using more than 2 threads doesn't seem to provide much benefit.)
    //
    /// Number of worker threads to use.
    #[usage(long, default = "2")]
    pub threads: NonZero<usize>,

    /// Exclude patterns
    #[usage(short, long)]
    pub exclude: Vec<String>,

    /// Search pattern.
    pub pattern: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn parses_defaults_and_repeated_excludes() {
        let argv = [
            OsStr::new("--exclude"),
            OsStr::new("target"),
            OsStr::new("--exclude"),
            OsStr::new(".git"),
            OsStr::new("needle"),
        ];

        let cli = Cli::parse_from(&argv).expect("valid locate arguments");

        assert_eq!(cli.limit.get(), 64);
        assert_eq!(cli.threads.get(), 2);
        assert_eq!(cli.exclude, ["target", ".git"]);
        assert_eq!(cli.pattern.as_deref(), Some("needle"));
    }

    #[test]
    fn rejects_unknown_flags() {
        let argv = [OsStr::new("--unknown")];

        assert!(matches!(
            Cli::parse_from(&argv),
            Err(usage::Error::UnknownFlag { .. })
        ));
    }
}
