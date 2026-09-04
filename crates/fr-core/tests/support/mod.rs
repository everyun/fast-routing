//! Where the upstream Freerouting tree is, for the tests that need one.
//!
//! The pre-rewrite fixture tests were written against `<repo>/upstream-freerouting/`, a
//! directory this repository does not vendor and `bench/fetch_fixtures.sh` does not create — so
//! every one of them either returned early (a silent skip) or, in
//! `kicad_roundtrip_test`'s case, asserted and went red. Neither is a test.
//!
//! This resolves the tree the way the rest of the repository does:
//!
//! | order | location |
//! |---|---|
//! | 1 | `$FR_UPSTREAM_DIR` |
//! | 2 | `$FR_FIXTURE_DIR`'s parent — the fetch script populates `<tree>/fixtures` |
//! | 3 | `/tmp/freerouting-upstream` |
//! | 4 | `<repo>/upstream-freerouting` |
//!
//! Callers that find nothing skip **loudly** rather than failing, so a missing
//! corpus does not turn optional fixture coverage into a hard red. The pipeline
//! corpus tests (`all_fixtures_parity_harness`, `pipeline_e2e_test`) do fail
//! loudly, which is gate G1.3.

#![allow(dead_code)] // each test binary uses a different subset

use std::path::{Path, PathBuf};

/// The upstream tree, or `None` with a printed reason.
pub fn upstream_tree() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::var_os("FR_UPSTREAM_DIR") {
        candidates.push(PathBuf::from(dir));
    }
    if let Some(dir) = std::env::var_os("FR_FIXTURE_DIR") {
        if let Some(parent) = Path::new(&dir).parent() {
            candidates.push(parent.to_path_buf());
        }
    }
    candidates.push(PathBuf::from("/tmp/freerouting-upstream"));
    candidates.push(repo_root().join("upstream-freerouting"));

    for candidate in &candidates {
        if candidate.join("fixtures").is_dir() {
            return Some(candidate.clone());
        }
    }
    eprintln!(
        "SKIP: no upstream tree with a fixtures/ directory in {candidates:?}; \
         run bench/fetch_fixtures.sh or set FR_UPSTREAM_DIR"
    );
    None
}

/// `<tree>/fixtures`, or `None`.
pub fn fixtures_dir() -> Option<PathBuf> {
    upstream_tree().map(|tree| tree.join("fixtures"))
}

/// The workspace root. `CARGO_MANIFEST_DIR` is `<root>/crates/fr-core`.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate is two levels below the workspace root")
        .to_path_buf()
}
