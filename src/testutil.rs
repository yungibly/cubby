//! Helpers for tests. Every temporary directory is created under the
//! crate's own `target/` directory so tests never touch the real home.

#![cfg(test)]

use std::path::PathBuf;

pub fn tmp_root() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/tmp");
    std::fs::create_dir_all(&root).unwrap();
    root
}

pub fn sandbox() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("cubby-test-")
        .tempdir_in(tmp_root())
        .unwrap()
}
