use std::io;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum CargoBinError {
    #[error("failed to read current exe")]
    CurrentExe {
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read current directory")]
    CurrentDir {
        #[source]
        source: std::io::Error,
    },
    #[error("CARGO_BIN_EXE env var {key} resolved to {path:?}, but it does not exist")]
    ResolvedPathDoesNotExist { key: String, path: PathBuf },
    #[error(
        "binary {name:?} was not built for this test run (looked for {expected:?}); \
         tests never build it themselves. Build it with the same profile and target as \
         the tests (`cargo build --bin {name}`), or export {env_key}=/absolute/path/to/{name}"
    )]
    NotFound {
        name: String,
        env_key: String,
        expected: PathBuf,
    },
}

/// Returns an absolute path to a binary target built for the current test run.
///
/// `CARGO_BIN_EXE_*` wins when set (nextest exports it for the test's own
/// package; CI exports it for cross-package binaries). Otherwise the binary
/// must already sit in the target directory beside the running test. This
/// never builds anything: a test that compiles a missing binary holds Cargo's
/// build lock for minutes and turns one setup mistake into a timeout per test.
pub fn cargo_bin(name: &str) -> Result<PathBuf, CargoBinError> {
    let env_keys = cargo_bin_env_keys(name);
    for key in &env_keys {
        if let Some(value) = std::env::var_os(key) {
            let path = PathBuf::from(&value);
            if path.is_absolute() && path.exists() {
                return Ok(path);
            }
            return Err(CargoBinError::ResolvedPathDoesNotExist {
                key: key.to_owned(),
                path,
            });
        }
    }

    let expected = target_dir_bin_path(name)?;
    if expected.exists() {
        return Ok(expected);
    }
    Err(CargoBinError::NotFound {
        name: name.to_owned(),
        env_key: format!("CARGO_BIN_EXE_{name}"),
        expected,
    })
}

fn cargo_bin_env_keys(name: &str) -> Vec<String> {
    let mut keys = Vec::with_capacity(2);
    keys.push(format!("CARGO_BIN_EXE_{name}"));

    // Cargo replaces dashes in target names when exporting env vars.
    let underscore_name = name.replace('-', "_");
    if underscore_name != name {
        keys.push(format!("CARGO_BIN_EXE_{underscore_name}"));
    }

    keys
}

/// `<target>/<profile>/<name>` for the profile and target the running test was
/// built with (test executables live in `<target>/<profile>/deps`).
fn target_dir_bin_path(name: &str) -> Result<PathBuf, CargoBinError> {
    let bin_name = format!("{}{}", name, std::env::consts::EXE_SUFFIX);
    let mut dir = std::env::current_exe().map_err(|source| CargoBinError::CurrentExe { source })?;
    dir.pop();
    if dir.ends_with("deps") {
        dir.pop();
    }
    Ok(dir.join(bin_name))
}

/// Macro that derives the path to a test resource at compile time using
/// `CARGO_MANIFEST_DIR`. This is expected to be used exclusively in test code.
#[macro_export]
macro_rules! find_resource {
    ($resource:expr) => {{
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        Ok::<std::path::PathBuf, std::io::Error>(manifest_dir.join($resource))
    }};
}

pub fn repo_root() -> io::Result<PathBuf> {
    let marker = Path::new(env!("CARGO_MANIFEST_DIR")).join("repo_root.marker");
    let mut root = marker;
    for _ in 0..4 {
        root = root
            .parent()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "repo_root.marker did not have expected parent depth",
                )
            })?
            .to_path_buf();
    }
    Ok(root)
}

#[cfg(test)]
mod tests;
