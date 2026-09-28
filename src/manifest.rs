//! The store manifest: `.cubby.toml` at the root of the store.
//!
//! The store's contents say *which files* are tracked. The manifest adds
//! what the contents alone cannot express: which directories are tracked as
//! a whole (so new files under them are picked up and deleted files are
//! dropped from the store), which patterns to ignore, and the permissions of
//! private files and directories, which git does not keep.
//!
//! It lives in the store so it is versioned and shared with it. cubby edits
//! it in place, so comments people write in it survive.

use std::collections::BTreeMap;
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
    /// Permissions of files and directories that group and others cannot
    /// read; see [`crate::perms`].
    pub modes: BTreeMap<Rel, u32>,
    /// The file as written, edited in place so comments survive.
    doc: DocumentMut,
    /// Every edit since loading, in order, so a run can record them and
    /// `cubby undo` can reverse exactly those.
    edits: Vec<Edit>,
}

/// One change to the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    AddDir(Rel),
    RemoveDir(Rel),
    AddIgnore(String),
    RemoveIgnore(String),
    Mode {
        rel: Rel,
        from: Option<u32>,
        to: Option<u32>,
    },
}

impl PartialEq for Manifest {
    fn eq(&self, other: &Manifest) -> bool {
        self.dirs == other.dirs && self.ignore == other.ignore && self.modes == other.modes
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
    #[serde(default)]
    modes: BTreeMap<String, RawMode>,
}

/// A mode as written: `"600"`, or a number such as `0o600`.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawMode {
    Text(String),
    Number(i64),
}

impl RawMode {
    fn bits(&self) -> Option<u32> {
        let bits = match self {
            RawMode::Text(t) if (3..=4).contains(&t.len()) => u32::from_str_radix(t, 8).ok()?,
            RawMode::Number(n) => u32::try_from(*n).ok()?,
            RawMode::Text(_) => return None,
        };
        (bits <= 0o7777).then_some(bits & 0o777)
    }
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
             # modes: permissions git cannot keep. cubby records the files and\n\
             #       directories that group and others cannot read when you save,\n\
             #       and restore applies them.\n\
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
        let mut modes = BTreeMap::new();
        for (path, mode) in raw.modes {
            let rel = Rel::parse(&path).with_context(|| format!("modes entry {path:?}"))?;
            let bits = mode.bits().with_context(|| {
                format!("modes entry {path:?}: a mode is three octal digits, like \"600\"")
            })?;
            modes.insert(rel, bits);
        }
        Ok(Manifest {
            dirs,
            ignore: raw.ignore,
            modes,
            doc,
            edits: Vec::new(),
        })
    }

    pub fn save(&mut self, store: &Path) -> Result<()> {
        let path = Self::path(store);
        fsx::write_atomic(&path, self.render().as_bytes())
            .with_context(|| format!("cannot write {}", path.display()))?;
        self.edits.clear();
        Ok(())
    }

    /// Edits made since loading or last saving.
    pub fn edits(&self) -> &[Edit] {
        &self.edits
    }

    /// Reverse `edit`, as far as the manifest still reflects it. Returns
    /// whether anything changed.
    pub fn reverse(&mut self, edit: &Edit) -> bool {
        match edit {
            Edit::AddDir(rel) => self.remove_dir(rel),
            Edit::RemoveDir(rel) => {
                matches!(self.add_dir(rel.clone()), AddDir::Added { .. })
            }
            Edit::AddIgnore(p) => self.remove_ignore(p),
            Edit::RemoveIgnore(p) => self.add_ignore(p),
            Edit::Mode { rel, from, to } => {
                self.modes.get(rel).copied() == *to && self.set_mode(rel, *from)
            }
        }
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
        self.edits
            .extend(absorbed.iter().cloned().map(Edit::RemoveDir));
        self.edits.push(Edit::AddDir(rel.clone()));

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

    /// Record the permissions of `rel`, or forget them with `None`. Returns
    /// whether anything changed.
    pub fn set_mode(&mut self, rel: &Rel, mode: Option<u32>) -> bool {
        let from = self.modes.get(rel).copied();
        if from == mode {
            return false;
        }
        self.edits.push(Edit::Mode {
            rel: rel.clone(),
            from,
            to: mode,
        });
        let key = rel.to_string();
        let table = self
            .doc
            .as_table_mut()
            .entry("modes")
            .or_insert_with(toml_edit::table)
            .as_table_mut()
            .expect("modes is a table");
        match mode {
            Some(m) => {
                self.modes.insert(rel.clone(), m);
                let is_new = !table.contains_key(&key);
                table.insert(&key, toml_edit::value(format!("{m:03o}")));
                if is_new {
                    table.sort_values();
                }
            }
            None => {
                self.modes.remove(rel);
                // Hand-written entries may spell the path differently.
                let keys: Vec<String> = table
                    .iter()
                    .map(|(k, _)| k.to_owned())
                    .filter(|k| Rel::parse(k).ok().as_ref() == Some(rel))
                    .collect();
                for k in keys {
                    table.remove(&k);
                }
            }
        }
        true
    }

    /// Add an ignore pattern at the end of the list. Returns false when it
    /// is already there.
    pub fn add_ignore(&mut self, pattern: &str) -> bool {
        if self.ignore.iter().any(|p| p == pattern) {
            return false;
        }
        self.ignore.push(pattern.to_owned());
        self.edits.push(Edit::AddIgnore(pattern.to_owned()));
        let array = tomlx::array_mut(&mut self.doc, "ignore").expect("ignore is a list");
        let end = array.len();
        tomlx::insert_str(array, end, pattern);
        true
    }

    /// Remove an ignore pattern. Returns false when it was not there.
    pub fn remove_ignore(&mut self, pattern: &str) -> bool {
        let before = self.ignore.len();
        self.ignore.retain(|p| p != pattern);
        if before == self.ignore.len() {
            return false;
        }
        self.edits.push(Edit::RemoveIgnore(pattern.to_owned()));
        let array = tomlx::array_mut(&mut self.doc, "ignore").expect("ignore is a list");
        tomlx::remove_where(array, |v| v.as_str() == Some(pattern));
        true
    }

    pub fn remove_dir(&mut self, rel: &Rel) -> bool {
        let before = self.dirs.len();
        self.dirs.retain(|d| d != rel);
        if before == self.dirs.len() {
            return false;
        }
        self.edits.push(Edit::RemoveDir(rel.clone()));
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
            m.add_ignore(n);
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
        m.add_ignore("*.bak");
        assert!(!m.add_ignore("*.bak"));
        m.remove_dir(&rel(".config/nvim"));
        let out = m.render();
        assert_eq!(
            out,
            "# my dotfiles\nversion = 2\n\n\
             dirs = [\n  \"~/.config/fish\",\n]\n\n\
             ignore = [\n  # plugin manager lock file\n  \"lazy-lock.json\",\n  \"*.bak\",\n]\n"
        );
        assert_eq!(Manifest::parse(&out).unwrap(), m);
        assert!(m.remove_ignore("lazy-lock.json"));
        assert!(!m.remove_ignore("lazy-lock.json"));
        assert!(!m.render().contains("plugin manager"));
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
    fn modes_round_trip_and_keep_their_comments() {
        let mut m = Manifest::parse(
            "dirs = []\n\n# private things\n[modes]\n# the netrc has a password\n\"~/.netrc\" = \"600\"\n",
        )
        .unwrap();
        assert_eq!(m.modes.get(&rel(".netrc")), Some(&0o600));
        assert!(m.set_mode(&rel(".ssh"), Some(0o700)));
        assert!(!m.set_mode(&rel(".ssh"), Some(0o700)));
        assert!(m.set_mode(&rel(".gnupg"), Some(0o700)));
        let out = m.render();
        assert!(
            out.contains(
                "# private things\n[modes]\n\"~/.gnupg\" = \"700\"\n# the netrc has a password\n\"~/.netrc\" = \"600\"\n\"~/.ssh\" = \"700\"\n"
            ),
            "{out}"
        );
        let parsed = Manifest::parse(&out).unwrap();
        assert_eq!(parsed, m);
        assert!(m.set_mode(&rel(".netrc"), None));
        assert!(!m.render().contains("netrc"));

        let m = Manifest::parse("[modes]\n'/.a' = 0o640\n\".b\" = \"0700\"\n").unwrap();
        assert_eq!(m.modes.get(&rel(".a")), Some(&0o640));
        assert_eq!(m.modes.get(&rel(".b")), Some(&0o700));
        assert!(Manifest::parse("[modes]\n\".a\" = \"rw\"\n").is_err());
        assert!(Manifest::parse("[modes]\n\".a\" = \"99999\"\n").is_err());
    }

    #[test]
    fn edits_are_logged_and_can_be_reversed() {
        let mut m = Manifest::parse("dirs = ['~/.config/nvim']\n").unwrap();
        m.add_dir(rel(".config"));
        m.add_ignore("*.bak");
        m.set_mode(&rel(".netrc"), Some(0o600));
        assert_eq!(
            m.edits(),
            &[
                Edit::RemoveDir(rel(".config/nvim")),
                Edit::AddDir(rel(".config")),
                Edit::AddIgnore("*.bak".into()),
                Edit::Mode {
                    rel: rel(".netrc"),
                    from: None,
                    to: Some(0o600)
                },
            ]
        );
        for e in m.edits().to_vec().iter().rev() {
            assert!(m.reverse(e), "{e:?}");
        }
        assert_eq!(m, Manifest::parse("dirs = ['~/.config/nvim']\n").unwrap());
        // A record changed since is left alone.
        m.set_mode(&rel(".netrc"), Some(0o640));
        let stale = Edit::Mode {
            rel: rel(".netrc"),
            from: None,
            to: Some(0o600),
        };
        assert!(!m.reverse(&stale));
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
