mod build_identity;

use std::{env, path::PathBuf};

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let workspace = manifest.parent().unwrap().parent().unwrap();
    let revision = build_identity::revision(workspace, &env::var("CARGO_PKG_VERSION").unwrap());
    println!("cargo:rustc-env=UPLC_BUILD_REVISION={revision}");

    // Deliberately absent: Cargo reruns this small script on every build. File
    // lists cannot detect newly untracked files, linked-worktree HEAD changes,
    // or disappearing Git metadata reliably. Recursively watching the repository
    // would also watch ignored target/build outputs. Rechecking avoids stale
    // clean identities at the small cost of rebuilding the independent core.
    let marker =
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("always-recheck-build-identity");
    println!("cargo:rerun-if-changed={}", marker.display());
}
