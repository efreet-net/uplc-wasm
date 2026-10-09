//! Build provenance shared by the build script and its integration tests.

use std::{path::Path, process::Command};

/// The revision is a build-time snapshot, including staged, unstaged, and
/// nonignored untracked files. Failed/missing Git metadata is never clean.
pub fn revision(workspace_root: &Path, version: &str) -> String {
    git_revision(workspace_root)
        .map(|revision| format!("{version}+git.{revision}"))
        .unwrap_or_else(|| format!("{version}+git.unknown"))
}

fn git_revision(workspace_root: &Path) -> Option<String> {
    // A source archive nested inside a different repository must not claim the
    // enclosing repository's commit as its own. `.git` can be a worktree file.
    if !workspace_root.join(".git").exists() {
        return None;
    }
    let root = workspace_root.canonicalize().ok()?;
    let reported_root = git(&root, &["rev-parse", "--show-toplevel"])?;
    if Path::new(reported_root.trim()).canonicalize().ok()? != root {
        return None;
    }
    let commit = git(&root, &["rev-parse", "--verify", "HEAD"])?;
    let commit = commit.trim();
    if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let status = git(
        &root,
        &[
            "status",
            "--porcelain=v1",
            "--untracked-files=normal",
            "--ignore-submodules=none",
        ],
    )?;
    Some(format!(
        "{commit}{}",
        if status.is_empty() { "" } else { ".dirty" }
    ))
}

fn git(root: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .current_dir(root)
        // Git's process-specific overrides must not redirect this source check
        // to another repository, index, or working tree.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(arguments)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).ok())?
}
