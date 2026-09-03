//! Walk the store and the tracked directories at home, and classify every
//! path into one [`State`].
//!
//! The store is walked in full (minus ignored paths); home is only walked
//! beneath the directories listed in the manifest. That is what makes "new
//! at home" a meaningful state: cubby only looks for new files where you
//! told it to.

use std::collections::BTreeMap;

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
    /// Could not be compared.
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

#[derive(Debug, Default)]
pub struct Scan {
    /// Sorted by path.
    pub entries: Vec<Entry>,
    /// Tracked directories that do not exist at home.
    pub absent_dirs: Vec<Rel>,
    /// Paths that were skipped, with the reason.
    pub notes: Vec<(Rel, String)>,
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
            present_dirs.push(dir.clone());
            self.walk_home_dir(dir, scope, store_meta.as_ref(), &mut home_side, &mut notes)?;
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
                    Kind::Other => notes.push((rel.clone(), "special file, skipped".into())),
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
            let state = self.classify(rel, home, store, dir.as_ref(), &present_dirs);
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

    fn walk_store(
        &self,
        scope: &Scope,
        notes: &mut Vec<(Rel, String)>,
    ) -> Result<BTreeMap<Rel, Meta>> {
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
                    if let Some(p) = e.path()
                        && let Ok(rel) = Rel::from_path_under(&self.layout.store, p)
                    {
                        notes.push((rel, format!("cannot read: {e}")));
                    }
                    continue;
                }
            };
            let rel = Rel::from_path_under(&self.layout.store, entry.path())?;
            if entry.file_type().is_dir() {
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
                    Kind::File | Kind::Symlink => {
                        found.insert(rel, meta);
                    }
                    Kind::Other => notes.push((rel, "special file in store, skipped".into())),
                    Kind::Dir => {}
                }
            }
        }
        Ok(found)
    }

    fn walk_home_dir(
        &self,
        dir: &Rel,
        scope: &Scope,
        store_meta: Option<&Meta>,
        found: &mut BTreeMap<Rel, Meta>,
        notes: &mut Vec<(Rel, String)>,
    ) -> Result<()> {
        let root = self.layout.live(dir);
        let mut walker = WalkDir::new(&root)
            .follow_links(false)
            .min_depth(1)
            .sort_by_file_name()
            .into_iter();
        while let Some(item) = walker.next() {
            let entry = match item {
                Ok(e) => e,
                Err(e) => {
                    if let Some(p) = e.path()
                        && let Ok(rel) = Rel::from_path_under(&self.layout.home, p)
                    {
                        notes.push((rel, format!("cannot read: {e}")));
                    }
                    continue;
                }
            };
            let rel = Rel::from_path_under(&self.layout.home, entry.path())?;
            if self.ignore.is_ignored(&rel) {
                if entry.file_type().is_dir() {
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
                    if scope.includes(&rel) {
                        found.insert(rel, meta);
                    }
                }
                Kind::Other => notes.push((rel, "special file, skipped".into())),
            }
        }
        Ok(())
    }

    fn classify(
        &self,
        rel: &Rel,
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
                            match fsx::same_content(
                                &self.layout.live(rel),
                                h,
                                &self.layout.stored(rel),
                                s,
                            ) {
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
            (Some(_), None) => State::New,
            (None, Some(_)) => State::Missing {
                deleted: dir.is_some_and(|d| present_dirs.contains(d)),
            },
            (None, None) => State::Error("vanished during scan".into()),
        }
    }
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
