//! Optional source identity for the top bar; never inspect the runtime workspace.

use std::path::{Path, PathBuf};
use std::process::Command;

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .current_dir(root)
        // Build identity must not be redirected by the caller's repository.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn watch(path: &Path) {
    // Watch the parent when a loose ref or packed-refs does not exist yet.
    // Unlike watching a missing file, this does not force every build to rerun.
    let mut existing = path;
    while !existing.exists() {
        let Some(parent) = existing.parent() else {
            return;
        };
        existing = parent;
    }
    println!("cargo::rerun-if-changed={}", existing.display());
}

fn source_sha(root: &Path) -> Option<String> {
    let dot_git = root.join(".git");
    watch(&dot_git);
    // Do not accidentally use an unrelated parent repo for an unpacked archive.
    if !dot_git.exists() {
        return None;
    }
    for name in ["HEAD", "packed-refs"] {
        if let Some(path) = git(
            root,
            &["rev-parse", "--path-format=absolute", "--git-path", name],
        ) {
            watch(Path::new(&path));
        }
    }
    if let Some(reference) = git(root, &["symbolic-ref", "-q", "HEAD"]) {
        if let Some(path) = git(
            root,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                &reference,
            ],
        ) {
            watch(Path::new(&path));
        }
    }
    let sha = git(root, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    ((sha.len() == 40 || sha.len() == 64) && sha.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some(sha)
}

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    let manifest =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest dir"));
    let root = manifest.join("../..");
    println!(
        "cargo::rustc-env=CHAOS_BUILD_SHA={}",
        source_sha(&root).unwrap_or_default()
    );
}
