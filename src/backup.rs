//! Copies of everything cubby overwrites or removes, kept under the state
//! directory as `backups/<timestamp>-<operation>/<path>`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::fsx::{self, Meta};
use crate::paths::Rel;

pub struct Backup {
    dir: PathBuf,
    count: usize,
}

impl Backup {
    pub fn new(state_dir: &Path, operation: &str) -> Backup {
        let stamp = jiff::Zoned::now().strftime("%Y%m%d-%H%M%S").to_string();
        let root = state_dir.join("backups");
        // Two runs within the same second get separate sets.
        let mut dir = root.join(format!("{stamp}-{operation}"));
        let mut n = 1;
        while dir.exists() {
            n += 1;
            dir = root.join(format!("{stamp}-{operation}-{n}"));
        }
        Backup { dir, count: 0 }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn count(&self) -> usize {
        self.count
    }

    /// Copy the file or symlink at `path` into the backup set.
    pub fn stash(&mut self, rel: &Rel, path: &Path, meta: &Meta) -> Result<()> {
        if meta.kind == fsx::Kind::Dir {
            return Ok(());
        }
        let dest = rel.under(&self.dir);
        fsx::copy_entry(path, meta, &dest, None)
            .with_context(|| format!("cannot back up {} to {}", path.display(), dest.display()))?;
        self.count += 1;
        Ok(())
    }
}

/// Delete the oldest backup sets so that at most `keep` remain. Returns how
/// many were removed.
pub fn prune(state_dir: &Path, keep: usize) -> Result<usize> {
    let root = state_dir.join("backups");
    let mut sets: Vec<PathBuf> = match std::fs::read_dir(&root) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.path())
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", root.display())),
    };
    // Names start with a timestamp, so lexical order is chronological.
    sets.sort();
    let mut removed = 0;
    while sets.len() > keep {
        let oldest = sets.remove(0);
        std::fs::remove_dir_all(&oldest)
            .with_context(|| format!("cannot remove {}", oldest.display()))?;
        removed += 1;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::sandbox;

    #[test]
    fn stash_and_prune() {
        let sb = sandbox();
        let state = sb.path().join("state");
        let file = sb.path().join("file");
        std::fs::write(&file, b"contents").unwrap();
        let meta = fsx::lstat(&file).unwrap().unwrap();
        let mut b = Backup::new(&state, "save");
        b.stash(&Rel::parse(".config/x").unwrap(), &file, &meta)
            .unwrap();
        assert_eq!(b.count(), 1);
        assert_eq!(
            std::fs::read(b.dir().join(".config/x")).unwrap(),
            b"contents"
        );

        for i in 0..5 {
            std::fs::create_dir_all(
                state
                    .join("backups")
                    .join(format!("20200101-00000{i}-save")),
            )
            .unwrap();
        }
        assert_eq!(prune(&state, 3).unwrap(), 3);
        let mut left: Vec<_> = std::fs::read_dir(state.join("backups"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left.len(), 3);
        assert!(left[0].starts_with("20200101-000003"));
        assert!(!left[2].starts_with("2020"), "the newest set is kept");
        assert_eq!(prune(&sb.path().join("nothing"), 3).unwrap(), 0);
    }
}
