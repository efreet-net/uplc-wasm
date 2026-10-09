#[path = "../build_identity.rs"]
mod build_identity;

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);

impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "uplc-build-identity-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args([
            "-c",
            "user.name=Identity Test",
            "-c",
            "user.email=identity@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn commit(root: &Path) -> String {
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "identity test snapshot"]);
    git(root, &["rev-parse", "HEAD"])
}

#[test]
fn clean_dirty_staged_untracked_and_unavailable_metadata_are_distinct() {
    let temp = Temp::new();
    let root = &temp.0;
    assert_eq!(build_identity::revision(root, "0.1.0"), "0.1.0+git.unknown");
    git(root, &["init", "-q", "--initial-branch=main"]);
    // A repository with no initial commit is still unknown.
    assert_eq!(build_identity::revision(root, "0.1.0"), "0.1.0+git.unknown");
    fs::write(root.join("tracked"), "initial").unwrap();
    fs::write(root.join(".gitignore"), "/ignored\n").unwrap();
    let head = commit(root);
    let clean = format!("0.1.0+git.{head}");
    let dirty = format!("{clean}.dirty");
    assert_eq!(build_identity::revision(root, "0.1.0"), clean);
    fs::write(root.join("ignored"), "not a source file").unwrap();
    assert_eq!(build_identity::revision(root, "0.1.0"), clean);
    fs::write(root.join("untracked"), "new source").unwrap();
    assert_eq!(build_identity::revision(root, "0.1.0"), dirty);
    fs::remove_file(root.join("untracked")).unwrap();
    fs::write(root.join("tracked"), "changed").unwrap();
    assert_eq!(build_identity::revision(root, "0.1.0"), dirty);
    git(root, &["add", "tracked"]);
    assert_eq!(build_identity::revision(root, "0.1.0"), dirty);
    let next = commit(root);
    assert_ne!(next, head);
    assert_eq!(
        build_identity::revision(root, "0.1.0"),
        format!("0.1.0+git.{next}")
    );

    let archive = root.join("unrelated-archive");
    fs::create_dir(&archive).unwrap();
    assert_eq!(
        build_identity::revision(&archive, "0.1.0"),
        "0.1.0+git.unknown"
    );
    // A malformed marker cannot turn missing metadata into a clean identity.
    fs::write(archive.join(".git"), "not a gitdir").unwrap();
    assert_eq!(
        build_identity::revision(&archive, "0.1.0"),
        "0.1.0+git.unknown"
    );
}

#[test]
fn linked_worktree_commit_and_dirty_state_use_the_worktree() {
    let temp = Temp::new();
    let main = temp.0.join("main");
    let linked = temp.0.join("linked");
    fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "--initial-branch=main"]);
    fs::write(main.join("source"), "first").unwrap();
    let head = commit(&main);
    git(
        &main,
        &["worktree", "add", "--detach", linked.to_str().unwrap()],
    );
    assert!(linked.join(".git").is_file());
    assert_eq!(
        build_identity::revision(&linked, "0.1.0"),
        format!("0.1.0+git.{head}")
    );
    fs::write(linked.join("source"), "linked change").unwrap();
    assert_eq!(
        build_identity::revision(&linked, "0.1.0"),
        format!("0.1.0+git.{head}.dirty")
    );
    let linked_head = commit(&linked);
    assert_ne!(linked_head, head);
    assert_eq!(
        build_identity::revision(&linked, "0.1.0"),
        format!("0.1.0+git.{linked_head}")
    );
    assert_eq!(
        build_identity::revision(&main, "0.1.0"),
        format!("0.1.0+git.{head}")
    );
}

#[test]
fn incremental_cargo_builds_refresh_head_dirty_and_missing_metadata() {
    let temp = Temp::new();
    let root = &temp.0;
    let package = root.join("crates/uplc-core");
    fs::create_dir_all(package.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = ['crates/uplc-core']\nresolver = '2'\n",
    )
    .unwrap();
    fs::write(
        package.join("Cargo.toml"),
        "[package]\nname = 'identity-probe'\nversion = '0.1.0'\nedition = '2024'\n",
    )
    .unwrap();
    fs::write(package.join("build.rs"), include_str!("../build.rs")).unwrap();
    fs::write(
        package.join("build_identity.rs"),
        include_str!("../build_identity.rs"),
    )
    .unwrap();
    fs::write(
        package.join("src/main.rs"),
        "fn main() { println!(\"{}\", env!(\"UPLC_BUILD_REVISION\")); }\n",
    )
    .unwrap();
    fs::write(root.join(".gitignore"), "/target/\n").unwrap();
    let build = || {
        let output = Command::new(env!("CARGO"))
            .current_dir(root)
            .env_remove("CARGO_TARGET_DIR")
            .args(["run", "--quiet", "--offline"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    assert_eq!(build(), "0.1.0+git.unknown");
    git(root, &["init", "-q", "--initial-branch=main"]);
    let head = commit(root);
    assert_eq!(build(), format!("0.1.0+git.{head}"));
    // The added file is outside the crate. Package file watching alone would
    // miss this transition, as well as its removal and a metadata-only commit.
    fs::write(root.join("new-source"), "changed").unwrap();
    assert_eq!(build(), format!("0.1.0+git.{head}.dirty"));
    fs::remove_file(root.join("new-source")).unwrap();
    assert_eq!(build(), format!("0.1.0+git.{head}"));
    git(root, &["commit", "--allow-empty", "-qm", "new HEAD"]);
    let next = git(root, &["rev-parse", "HEAD"]);
    assert_ne!(head, next);
    assert_eq!(build(), format!("0.1.0+git.{next}"));
    fs::remove_dir_all(root.join(".git")).unwrap();
    assert_eq!(build(), "0.1.0+git.unknown");
}

#[test]
fn native_server_and_shared_api_report_the_same_build_snapshot() {
    let json: serde_json::Value = serde_json::from_str(&uplc_core::evaluate_json("{}")).unwrap();
    assert_eq!(json["revision"], uplc_core::BUILD_REVISION);
    assert!(uplc_core::BUILD_REVISION.starts_with(concat!(env!("CARGO_PKG_VERSION"), "+git.")));
    let mut native = Command::new(env!("CARGO_BIN_EXE_uplc-native"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    native.stdin.take().unwrap().write_all(b"{}\n").unwrap();
    let output = native.wait_with_output().unwrap();
    assert!(output.status.success());
    let native: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(native, json);
}
