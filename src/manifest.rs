//! The store manifest: `.cubby.toml` at the root of the store.
//!
//! The store's contents say *which files* are tracked. The manifest adds the
//! two things the contents alone cannot express: which directories are
//! tracked as a whole (so new files under them are picked up and deleted
//! files are dropped from the store), and which patterns to ignore.
//!
//! It lives in the store so it is versioned and shared with it. cubby edits
//! it in place, so comments people write in it survive.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use toml_edit::DocumentMut;

use crate::config::toml_string;
use crate::fsx;
use crate::paths::Rel;
use crate::tomlx;

pub const FILE_NAME: &str = ".cubby.toml";

/// The manifest format this cubby writes. Version 1 is the unversioned
/// format of cubby 2.
pub const VERSION: i64 = 2;

/// Ignore patterns written into a fresh manifest.
pub const DEFAULT_IGNORE: &[&str] = &["*.swp", "*~", "__pycache__", "node_modules"];

#[derive(Debug, Clone)]
pub struct Manifest {
    /// Directories tracked as a whole, in normalized home-relative form.
    pub dirs: Vec<Rel>,
    /// Ignore patterns; see [`crate::ignore`] for the syntax.
    pub ignore: Vec<String>,
    /// The file as written, edited in place so comments survive.
    doc: DocumentMut,
}

impl PartialEq for Manifest {
    fn eq(&self, other: &Manifest) -> bool {
        self.dirs == other.dirs && self.ignore == other.ignore
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Raw {
    #[serde(default)]
    #[allow(dead_code)] // checked before deserializing
    version: Option<i64>,
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

impl Default for Manifest {
    /// A manifest with nothing in it, for a store that has none.
    fn default() -> Manifest {
        Manifest::from_template(&[])
    }
}

impl Manifest {
    /// The manifest `cubby init` writes.
    pub fn fresh() -> Manifest {
        Manifest::from_template(DEFAULT_IGNORE)
    }

    fn from_template(ignore: &[&str]) -> Manifest {
        let mut text = String::from(
            "# cubby manifest. Lives in the store and travels with it.\n\
             #\n\
             # dirs: directories tracked as a whole. `cubby` picks up new files under\n\
             #       them and drops files from the store that you deleted at home.\n\
             # ignore: never tracked. A pattern without a slash matches a file or\n\
             #       directory name at any depth; one with a slash matches a path\n\
             #       relative to your home directory (`**` is allowed).\n\
             #\n\
             # cubby keeps your comments when it updates this file.\n\n",
        );
        text.push_str(&format!(
            "version = {VERSION}\n\ndirs = [\n]\n\nignore = [\n"
        ));
        for p in ignore {
            text.push_str(&format!("  {},\n", toml_string(p)));
        }
        text.push_str("]\n");
        Manifest::parse(&text).expect("the manifest template parses")
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
        let doc: DocumentMut = text.parse()?;
        // Check the version before anything else, so a store written by a
        // newer cubby says so instead of failing on a key this one does
        // not know.
        if let Some(item) = doc.get("version") {
            match item.as_integer() {
                Some(v) if v > VERSION => bail!(
                    "this store was written by a newer cubby (manifest version {v}; this cubby reads up to {VERSION}); upgrade cubby"
                ),
                Some(v) if v >= 1 => {}
                _ => bail!("version must be a whole number, 1 or more"),
            }
        }
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
            doc,
        })
    }

    pub fn save(&self, store: &Path) -> Result<()> {
        let path = Self::path(store);
        fsx::write_atomic(&path, self.render().as_bytes())
            .with_context(|| format!("cannot write {}", path.display()))
    }

    /// The file's text, marked with the format version it now uses.
    pub fn render(&self) -> String {
        let mut doc = self.doc.clone();
        tomlx::set_first(&mut doc, "version", toml_edit::value(VERSION));
        doc.to_string()
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
        self.dirs.push(rel.clone());
        self.dirs.sort();

        let array = tomlx::array_mut(&mut self.doc, "dirs").expect("dirs is a list");
        tomlx::remove_where(array, |v| parses_within(v, &rel));
        // Keep the list sorted: before the first entry that sorts after it.
        let index = array
            .iter()
            .position(|v| {
                v.as_str()
                    .and_then(|s| Rel::parse(s).ok())
                    .is_some_and(|d| d > rel)
            })
            .unwrap_or(array.len());
        tomlx::insert_str(array, index, &rel.to_string());
        AddDir::Added { absorbed }
    }

    pub fn remove_dir(&mut self, rel: &Rel) -> bool {
        let before = self.dirs.len();
        self.dirs.retain(|d| d != rel);
        if before == self.dirs.len() {
            return false;
        }
        let array = tomlx::array_mut(&mut self.doc, "dirs").expect("dirs is a list");
        tomlx::remove_where(array, |v| {
            v.as_str().and_then(|s| Rel::parse(s).ok()).as_ref() == Some(rel)
        });
        true
    }
}

/// Whether an array element names `rel` or a directory inside it.
fn parses_within(v: &toml_edit::Value, rel: &Rel) -> bool {
    v.as_str()
        .and_then(|s| Rel::parse(s).ok())
        .is_some_and(|d| d.is_within(rel))
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
        assert!(m.render().contains("version = 2\n"));
    }

    #[test]
    fn round_trip_keeps_any_name() {
        let names = [
            "notes-\u{1F469}\u{200D}\u{1F4BB}",
            "\u{301}starts-with-a-mark",
            "zero\u{200B}width",
            "tab\there",
            "esc\u{1b}x",
            "del\u{7f}x",
            "new\nline",
            "quote\"and\\backslash",
            "it's",
            " padded ",
        ];
        let mut m = Manifest::fresh();
        for n in names {
            m.add_dir(rel(&format!(".config/{n}")));
        }
        let parsed = Manifest::parse(&m.render()).unwrap();
        assert_eq!(parsed, m);
    }

    #[test]
    fn comments_survive_edits() {
        let text = "# my dotfiles\n\
                    dirs = [\n  \"~/.config/nvim\",\n]\n\n\
                    ignore = [\n  # plugin manager lock file\n  \"lazy-lock.json\",\n]\n";
        let mut m = Manifest::parse(text).unwrap();
        m.add_dir(rel(".config/fish"));
        m.remove_dir(&rel(".config/nvim"));
        let out = m.render();
        assert_eq!(
            out,
            "# my dotfiles\nversion = 2\n\n\
             dirs = [\n  \"~/.config/fish\",\n]\n\n\
             ignore = [\n  # plugin manager lock file\n  \"lazy-lock.json\",\n]\n"
        );
        assert_eq!(Manifest::parse(&out).unwrap(), m);
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
    fn newer_versions_ask_for_an_upgrade() {
        let err = Manifest::parse("version = 99\nshiny = true\n").unwrap_err();
        assert!(err.to_string().contains("upgrade cubby"), "{err}");
        assert!(Manifest::parse("version = 1\ndirs = []\n").is_ok());
        assert!(Manifest::parse("version = 'two'\n").is_err());
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
        assert_eq!(Manifest::parse(&m.render()).unwrap().dirs, m.dirs);
        assert_eq!(m.dir_for(&rel(".config/foo/bar")), Some(&rel(".config")));
        assert_eq!(m.dir_for(&rel(".configx")), None);
        assert!(m.remove_dir(&rel(".config")));
        assert!(!m.remove_dir(&rel(".config")));
        assert!(Manifest::parse(&m.render()).unwrap().dirs.is_empty());
    }
}
