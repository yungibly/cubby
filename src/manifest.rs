//! The store manifest: `.cubby.toml` at the root of the store.
//!
//! The store's contents say *which files* are tracked. The manifest adds the
//! two things the contents alone cannot express: which directories are
//! tracked as a whole (so new files under them are picked up and deleted
//! files are dropped from the store), and which patterns to ignore.
//!
//! It lives in the store so it is versioned and shared with it.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::fsx;
use crate::paths::Rel;

pub const FILE_NAME: &str = ".cubby.toml";

/// Ignore patterns written into a fresh manifest.
pub const DEFAULT_IGNORE: &[&str] = &["*.swp", "*~", "__pycache__", "node_modules"];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    /// Directories tracked as a whole, in normalized home-relative form.
    pub dirs: Vec<Rel>,
    /// Ignore patterns; see [`crate::ignore`] for the syntax.
    pub ignore: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Raw {
    #[serde(default)]
    dirs: Vec<String>,
    #[serde(default)]
    ignore: Vec<String>,
}

/// What happened when a directory was added.
#[derive(Debug, PartialEq, Eq)]
pub enum AddDir {
    /// Now tracked; lists any previously tracked directories inside it that
    /// it absorbed.
    Added { absorbed: Vec<Rel> },
    /// Already inside this tracked directory, so nothing changed.
    Covered(Rel),
}

impl Manifest {
    pub fn fresh() -> Manifest {
        Manifest {
            dirs: Vec::new(),
            ignore: DEFAULT_IGNORE.iter().map(|s| s.to_string()).collect(),
        }
    }

    pub fn path(store: &Path) -> std::path::PathBuf {
        store.join(FILE_NAME)
    }

    /// Load the manifest, or an empty one when the store has none yet.
    pub fn load(store: &Path) -> Result<Manifest> {
        let path = Self::path(store);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Manifest::default()),
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
        };
        Self::parse(&text).with_context(|| format!("invalid manifest {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Manifest> {
        let raw: Raw = toml::from_str(text)?;
        let mut dirs = Vec::new();
        for d in raw.dirs {
            let rel = Rel::parse(&d).with_context(|| format!("dirs entry {d:?}"))?;
            if !dirs.contains(&rel) {
                dirs.push(rel);
            }
        }
        dirs.sort();
        Ok(Manifest {
            dirs,
            ignore: raw.ignore,
        })
    }

    pub fn save(&self, store: &Path) -> Result<()> {
        let path = Self::path(store);
        fsx::write_atomic(&path, self.render().as_bytes())
            .with_context(|| format!("cannot write {}", path.display()))
    }

    pub fn render(&self) -> String {
        let mut out = String::from(
            "# cubby manifest. Lives in the store and travels with it.\n\
             #\n\
             # dirs: directories tracked as a whole. `cubby` picks up new files under\n\
             #       them and drops files from the store that you deleted at home.\n\
             # ignore: never tracked. A pattern without a slash matches a file or\n\
             #       directory name at any depth; one with a slash matches a path\n\
             #       relative to your home directory (`**` is allowed).\n\
             #\n\
             # cubby rewrites this file when tracked directories change, so keep\n\
             # your own notes in the store's README rather than here.\n\n",
        );
        out.push_str("dirs = [\n");
        for d in &self.dirs {
            out.push_str(&format!("  {:?},\n", d.to_string()));
        }
        out.push_str("]\n\nignore = [\n");
        for p in &self.ignore {
            out.push_str(&format!("  {p:?},\n"));
        }
        out.push_str("]\n");
        out
    }

    /// The tracked directory that contains `rel` (or is `rel`), if any.
    pub fn dir_for(&self, rel: &Rel) -> Option<&Rel> {
        self.dirs.iter().find(|d| rel.is_within(d))
    }

    pub fn is_dir(&self, rel: &Rel) -> bool {
        self.dirs.contains(rel)
    }

    /// Start tracking a directory. A directory inside one already tracked is
    /// a no-op; one that contains tracked directories absorbs them.
    pub fn add_dir(&mut self, rel: Rel) -> AddDir {
        if let Some(existing) = self.dir_for(&rel) {
            return AddDir::Covered(existing.clone());
        }
        let absorbed: Vec<Rel> = self
            .dirs
            .iter()
            .filter(|d| d.is_within(&rel))
            .cloned()
            .collect();
        self.dirs.retain(|d| !d.is_within(&rel));
        self.dirs.push(rel);
        self.dirs.sort();
        AddDir::Added { absorbed }
    }

    pub fn remove_dir(&mut self, rel: &Rel) -> bool {
        let before = self.dirs.len();
        self.dirs.retain(|d| d != rel);
        before != self.dirs.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(s: &str) -> Rel {
        Rel::parse(s).unwrap()
    }

    #[test]
    fn round_trip() {
        let mut m = Manifest::fresh();
        m.add_dir(rel("~/.config/nvim"));
        m.add_dir(rel(".config/fish"));
        let parsed = Manifest::parse(&m.render()).unwrap();
        assert_eq!(parsed, m);
        assert_eq!(parsed.dirs, vec![rel(".config/fish"), rel(".config/nvim")]);
    }

    #[test]
    fn parse_accepts_hand_written_forms() {
        let m = Manifest::parse("dirs = ['~/.config/nvim', '/.config/nvim/', '.ssh']\n").unwrap();
        assert_eq!(m.dirs, vec![rel(".config/nvim"), rel(".ssh")]);
        assert!(m.ignore.is_empty());
        assert!(Manifest::parse("dirs = ['../x']").is_err());
        assert!(Manifest::parse("dir = []").is_err());
    }

    #[test]
    fn add_dir_absorbs_and_covers() {
        let mut m = Manifest::default();
        assert_eq!(
            m.add_dir(rel(".config/nvim")),
            AddDir::Added { absorbed: vec![] }
        );
        assert_eq!(
            m.add_dir(rel(".config/nvim/lua")),
            AddDir::Covered(rel(".config/nvim"))
        );
        assert_eq!(
            m.add_dir(rel(".config")),
            AddDir::Added {
                absorbed: vec![rel(".config/nvim")]
            }
        );
        assert_eq!(m.dirs, vec![rel(".config")]);
        assert_eq!(m.dir_for(&rel(".config/foo/bar")), Some(&rel(".config")));
        assert_eq!(m.dir_for(&rel(".configx")), None);
        assert!(m.remove_dir(&rel(".config")));
        assert!(!m.remove_dir(&rel(".config")));
    }
}
