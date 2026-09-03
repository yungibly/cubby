//! Walk the store and the tracked directories at home, and classify every
//! path into one [`State`].
//!
//! The store is walked in full (minus ignored paths); home is only walked
//! beneath the directories listed in the manifest. That is what makes "new
//! at home" a meaningful state: cubby only looks for new files where you
//! told it to.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

use anyhow::Result;
use walkdir::WalkDir;

use crate::fsx::{self, Kind, Meta};
use crate::ignore::Ignore;
use crate::manifest::Manifest;
use crate::paths::{Layout, Rel};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Newer {
    Home,
    Store,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Identical on both sides.
    Same,
    /// Present on both sides with different content (or executable bit).
    Modified(Newer),
    /// Only at home. Under a tracked directory this means "not saved yet";
    /// elsewhere it means "not tracked".
    New,
    /// Only in the store. `deleted` is true when it lies under a tracked
    /// directory that exists at home, meaning it was deleted there.
    Missing { deleted: bool },
    /// Present on both sides but as different kinds of thing.
    Conflict { home: Kind, store: Kind },
    /// Could not be compared, or could not be read.
    Error(String),
}

impl State {
    pub fn is_same(&self) -> bool {
        matches!(self, State::Same)
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub rel: Rel,
    pub state: State,
    pub home: Option<Meta>,
    pub store: Option<Meta>,
    /// The tracked directory this path lies under, if any.
    pub dir: Option<Rel>,
}

/// Something that was skipped, and why.
#[derive(Clone, Debug)]
pub struct Note {
    /// A path for display; it may not be a valid [`Rel`].
    pub path: String,
    pub why: String,
}

#[derive(Debug, Default)]
pub struct Scan {
    /// Sorted by path.
    pub entries: Vec<Entry>,
    /// Tracked directories that do not exist at home.
    pub absent_dirs: Vec<Rel>,
    /// Tracked directories that exist at home but hold no files while the
    /// store has some. Treated like absent ones: an empty directory is far
    /// more likely an unmounted volume or a wiped config than a deliberate
    /// deletion of every file.
    pub empty_dirs: Vec<Rel>,
    pub notes: Vec<Note>,
}

/// Which paths a command is interested in. Empty means everything.
#[derive(Clone, Debug, Default)]
pub struct Scope {
    pub rels: Vec<Rel>,
}

impl Scope {
    pub fn all() -> Scope {
        Scope::default()
    }

    pub fn of(rels: Vec<Rel>) -> Scope {
        Scope { rels }
    }

    pub fn is_all(&self) -> bool {
        self.rels.is_empty()
    }

    /// Whether a file at `rel` is inside the scope.
    pub fn includes(&self, rel: &Rel) -> bool {
        self.rels.is_empty() || self.rels.iter().any(|p| rel.is_within(p))
    }

    /// Whether walking into directory `dir` could reach something in scope.
    pub fn may_descend(&self, dir: &Rel) -> bool {
        self.rels.is_empty()
            || self
                .rels
                .iter()
                .any(|p| p.is_within(dir) || dir.is_within(p))
    }
}

pub struct Scanner<'a> {
    pub layout: &'a Layout,
    pub manifest: &'a Manifest,
    pub ignore: &'a Ignore,
}

impl Scanner<'_> {
    pub fn scan(&self, scope: &Scope) -> Result<Scan> {
        let mut notes = Vec::new();
        let store_side = self.walk_store(scope, &mut notes)?;

        let mut home_side: BTreeMap<Rel, Meta> = BTreeMap::new();
        let mut absent_dirs = Vec::new();
        let mut empty_dirs = Vec::new();
        let mut present_dirs = Vec::new();
        let store_meta = fsx::lstat(&self.layout.store)?;

        for dir in &self.manifest.dirs {
            if !scope.may_descend(dir) {
                continue;
            }
            let root = self.layout.live(dir);
            if !root.is_dir() {
                absent_dirs.push(dir.clone());
                continue;
            }
            let seen =
                self.walk_home_dir(dir, scope, store_meta.as_ref(), &mut home_side, &mut notes)?;
            if seen == 0 && store_side.keys().any(|r| r.is_within(dir)) {
                empty_dirs.push(dir.clone());
            } else {
                present_dirs.push(dir.clone());
            }
        }

        // Everything in the store that the directory walks did not find is
        // looked up at home individually: files tracked on their own, and
        // files deleted from a tracked directory (which may have been
        // replaced by something of another kind).
        for rel in store_side.keys() {
            if home_side.contains_key(rel) {
                continue;
            }
            if let Some(meta) = fsx::lstat(&self.layout.live(rel))? {
                home_side.insert(rel.clone(), meta);
            }
        }

        // Explicitly named files are looked at even outside tracked
        // directories, so `cubby save ~/.zshrc` sees a file that is not in
        // the store yet.
        for rel in &scope.rels {
            if home_side.contains_key(rel) || self.ignore.is_ignored(rel) {
                continue;
            }
            if let Some(meta) = fsx::lstat(&self.layout.live(rel))? {
                match meta.kind {
                    Kind::File | Kind::Symlink => {
                        home_side.insert(rel.clone(), meta);
                    }
                    Kind::Dir => {}
                    Kind::Other => notes.push(Note {
                        path: rel.to_string(),
                        why: "special file, skipped".into(),
                    }),
                }
            }
        }

        let mut entries = Vec::new();
        let mut keys: Vec<&Rel> = store_side.keys().chain(home_side.keys()).collect();
        keys.sort();
        keys.dedup();
        for rel in keys {
            let home = home_side.get(rel);
            let store = store_side.get(rel);
            let dir = self.manifest.dir_for(rel).cloned();
            let state = classify(home, store, dir.as_ref(), &present_dirs);
            entries.push(Entry {
                rel: rel.clone(),
                state,
                home: home.cloned(),
                store: store.cloned(),
                dir,
            });
        }

        Ok(Scan {
            entries,
            absent_dirs,
            empty_dirs,
            notes,
        })
    }

    /// Every file and symlink in the store, ignoring nothing but ignored
    /// paths. Used by `list`.
    pub fn store_entries(&self) -> Result<Vec<(Rel, Meta)>> {
        let mut notes = Vec::new();
        Ok(self
            .walk_store(&Scope::all(), &mut notes)?
            .into_iter()
            .collect())
    }

    fn walk_store(&self, scope: &Scope, notes: &mut Vec<Note>) -> Result<BTreeMap<Rel, Meta>> {
        let mut found = BTreeMap::new();
        if !self.layout.store.is_dir() {
            return Ok(found);
        }
        let mut walker = WalkDir::new(&self.layout.store)
            .follow_links(false)
            .min_depth(1)
            .sort_by_file_name()
            .into_iter();
        while let Some(item) = walker.next() {
            let entry = match item {
                Ok(e) => e,
                Err(e) => {
                    notes.push(walk_error(&self.layout.store, "store", &e));
                    continue;
                }
            };
            let is_dir = entry.file_type().is_dir();
            let rel = match Rel::from_path_under(&self.layout.store, entry.path()) {
                Ok(rel) => rel,
                Err(e) => {
                    notes.push(Note {
                        path: format!("store/{}", lossy(&self.layout.store, entry.path())),
                        why: format!("skipped: {e}"),
                    });
                    if is_dir {
                        walker.skip_current_dir();
                    }
                    continue;
                }
            };
            if is_dir {
                if self.ignore.is_ignored(&rel) || !scope.may_descend(&rel) {
                    walker.skip_current_dir();
                }
                continue;
            }
            if self.ignore.is_ignored(&rel) || !scope.includes(&rel) {
                continue;
            }
            if let Some(meta) = fsx::lstat(entry.path())? {
                match meta.kind {
                    Kind::File | Kind::Symlink => insert(&mut found, rel, meta, notes),
                    Kind::Other => notes.push(Note {
                        path: format!("store/{}", rel.as_str()),
                        why: "special file, skipped".into(),
                    }),
                    Kind::Dir => {}
                }
            }
        }
        Ok(found)
    }

    /// Walk a tracked directory at home. Returns how many files and symlinks
    /// were seen (ignored ones excluded), whether or not they were in scope.
    fn walk_home_dir(
        &self,
        dir: &Rel,
        scope: &Scope,
        store_meta: Option<&Meta>,
        found: &mut BTreeMap<Rel, Meta>,
        notes: &mut Vec<Note>,
    ) -> Result<usize> {
        let root = self.layout.live(dir);
        let mut seen = 0;
        let mut walker = WalkDir::new(&root)
            .follow_links(false)
            .min_depth(1)
            .sort_by_file_name()
            .into_iter();
        while let Some(item) = walker.next() {
            let entry = match item {
                Ok(e) => e,
                Err(e) => {
                    notes.push(walk_error(&self.layout.home, "~", &e));
                    continue;
                }
            };
            let is_dir = entry.file_type().is_dir();
            let rel = match Rel::from_path_under(&self.layout.home, entry.path()) {
                Ok(rel) => rel,
                Err(e) => {
                    notes.push(Note {
                        path: format!("~/{}", lossy(&self.layout.home, entry.path())),
                        why: format!("skipped: {e}"),
                    });
                    if is_dir {
                        walker.skip_current_dir();
                    }
                    continue;
                }
            };
            if self.ignore.is_ignored(&rel) {
                if is_dir {
                    walker.skip_current_dir();
                }
                continue;
            }
            let Some(meta) = fsx::lstat(entry.path())? else {
                continue;
            };
            match meta.kind {
                Kind::Dir => {
                    let is_store = store_meta.is_some_and(|s| fsx::same_inode(s, &meta));
                    if is_store || !scope.may_descend(&rel) {
                        walker.skip_current_dir();
                    }
                }
                Kind::File | Kind::Symlink => {
                    seen += 1;
                    if scope.includes(&rel) {
                        insert(found, rel, meta, notes);
                    }
                }
                Kind::Other => notes.push(Note {
                    path: rel.to_string(),
                    why: "special file, skipped".into(),
                }),
            }
        }
        Ok(seen)
    }
}

/// Insert into a side map, noting a second on-disk name that normalizes to
/// the same path (possible on filesystems that keep both Unicode forms).
fn insert(map: &mut BTreeMap<Rel, Meta>, rel: Rel, meta: Meta, notes: &mut Vec<Note>) {
    if let Some(existing) = map.get(&rel) {
        notes.push(Note {
            path: rel.to_string(),
            why: format!(
                "{} and {} are the same name in different Unicode forms; using the first",
                existing.path.display(),
                meta.path.display()
            ),
        });
        return;
    }
    map.insert(rel, meta);
}

fn walk_error(base: &Path, label: &str, e: &walkdir::Error) -> Note {
    let path = e
        .path()
        .map(|p| format!("{label}/{}", lossy(base, p)))
        .unwrap_or_else(|| label.to_owned());
    Note {
        path,
        why: format!(
            "cannot read: {}",
            e.io_error()
                .map(|io| io.to_string())
                .unwrap_or_else(|| e.to_string())
        ),
    }
}

fn lossy(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn classify(
    home: Option<&Meta>,
    store: Option<&Meta>,
    dir: Option<&Rel>,
    present_dirs: &[Rel],
) -> State {
    match (home, store) {
        (Some(h), Some(s)) if h.kind != s.kind => State::Conflict {
            home: h.kind,
            store: s.kind,
        },
        (Some(h), Some(s)) => {
            let equal = match h.kind {
                Kind::Symlink => h.target == s.target,
                _ => {
                    if h.is_executable() != s.is_executable() {
                        false
                    } else {
                        match fsx::same_content(&h.path, h, &s.path, s) {
                            Ok(eq) => eq,
                            Err(e) => return State::Error(format!("{e:#}")),
                        }
                    }
                }
            };
            if equal {
                State::Same
            } else {
                State::Modified(newer(h, s))
            }
        }
        (Some(h), None) => unreadable(h).unwrap_or(State::New),
        (None, Some(s)) => unreadable(s).unwrap_or(State::Missing {
            deleted: dir.is_some_and(|d| present_dirs.contains(d)),
        }),
        (None, None) => State::Error("vanished during scan".into()),
    }
}

/// An error state when a regular file cannot be opened for reading, since
/// copying it would fail anyway.
fn unreadable(meta: &Meta) -> Option<State> {
    if meta.kind != Kind::File {
        return None;
    }
    File::open(&meta.path)
        .err()
        .map(|e| State::Error(format!("cannot read: {e}")))
}

fn newer(home: &Meta, store: &Meta) -> Newer {
    let secs = |m: &Meta| {
        m.mtime
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    };
    match secs(home).cmp(&secs(store)) {
        std::cmp::Ordering::Greater => Newer::Home,
        std::cmp::Ordering::Less => Newer::Store,
        std::cmp::Ordering::Equal => Newer::Unknown,
    }
}
