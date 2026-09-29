//! Walk the store and the tracked directories at home, and classify every
//! path into one [`State`].
//!
//! The store is walked in full (minus ignored paths); home is only walked
//! beneath the directories listed in the manifest. That is what makes "new
//! at home" a meaningful state: cubby only looks for new files where you
//! told it to.
//!
//! Files are compared by fingerprint, and each side is compared with the
//! baseline in the [`Index`] (what both sides held the last time they
//! matched) to tell which side changed.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use anyhow::Result;
use walkdir::WalkDir;

use crate::fsx::{self, Kind, Meta};
use crate::ignore::Ignore;
use crate::index::{self, Base, Fp, Index};
use crate::manifest::Manifest;
use crate::paths::{Layout, Rel, Side};

/// Which side of a modified path changed since the last sync.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Home,
    Store,
    Both,
    /// No record of the last sync (a new machine, or a path cubby has not
    /// seen both sides of).
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Identical on both sides.
    Same,
    /// Present on both sides with different content (or executable bit).
    Modified(Change),
    /// Only at home. Under a tracked directory this means "not saved yet";
    /// elsewhere it means "not tracked". `was_stored`: it was in the store
    /// at the last sync, so it has been deleted from the store since.
    New { was_stored: bool },
    /// Only in the store.
    Missing {
        /// It was at home at the last sync, so it was deleted at home.
        was_here: bool,
        /// It lies under a tracked directory that exists at home, so a
        /// deletion there is one to mirror.
        under_present_dir: bool,
        /// The store copy changed since the last sync.
        store_changed: bool,
    },
    /// Present on both sides but as different kinds of thing.
    Conflict { home: Kind, store: Kind },
    /// Could not be compared, or could not be read.
    Error(String),
}

impl State {
    pub fn is_same(&self) -> bool {
        matches!(self, State::Same)
    }

    /// A new file that is not tracked: at home, outside tracked
    /// directories, named on the command line.
    pub fn is_untracked(&self, dir: Option<&Rel>) -> bool {
        matches!(self, State::New { .. }) && dir.is_none()
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
    /// Fingerprints, where they were read or cached.
    pub home_fp: Option<Fp>,
    pub store_fp: Option<Fp>,
}

/// A tracked file or directory whose permissions at home differ from what
/// the manifest records (see [`crate::perms`]).
#[derive(Clone, Debug)]
pub struct PermDiff {
    pub rel: Rel,
    pub is_dir: bool,
    /// The home path, for changing its permissions.
    pub path: PathBuf,
    /// The permission bits at home.
    pub home: u32,
    pub recorded: Option<u32>,
    /// What a save would record.
    pub record: Option<u32>,
    /// What a restore would leave at home.
    pub restored: u32,
}

impl PermDiff {
    pub fn needs_record(&self) -> bool {
        self.record != self.recorded
    }

    pub fn needs_chmod(&self) -> bool {
        self.restored != self.home
    }
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
    /// Permission differences, files and directories, sorted by path.
    pub perms: Vec<PermDiff>,
    /// The permission bits of every home directory that holds a tracked
    /// path, for creating their counterparts in the store.
    pub dir_modes: BTreeMap<Rel, u32>,
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
    /// Baselines and cached fingerprints.
    pub index: &'a Index,
    /// Device and inode of the store, cubby's state directory, and its
    /// config file: never walked into or copied, even through a symlinked
    /// path that the ignore rules would not recognise.
    pub own: Vec<(u64, u64)>,
}

impl Scanner<'_> {
    pub fn scan(&self, scope: &Scope) -> Result<Scan> {
        let mut notes = Vec::new();
        let store_side = self.walk_store(scope, &mut notes)?;

        let mut home_side: BTreeMap<Rel, Meta> = BTreeMap::new();
        let mut absent_dirs = Vec::new();
        let mut empty_dirs = Vec::new();
        let mut present_dirs = Vec::new();

        for dir in &self.manifest.dirs {
            if !scope.may_descend(dir) {
                continue;
            }
            let root = self.layout.live(dir);
            if !root.is_dir() {
                absent_dirs.push(dir.clone());
                continue;
            }
            let seen = self.walk_home_dir(dir, scope, &mut home_side, &mut notes)?;
            // The walk skips parts out of scope, so before calling the
            // directory empty, make sure nothing is anywhere in it.
            if seen == 0 && store_side.keys().any(|r| r.is_within(dir)) && !self.holds_files(dir) {
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
            let under_present_dir = dir.as_ref().is_some_and(|d| present_dirs.contains(d));
            let base = self.index.base(rel);
            let c = self.classify(rel, home, store, base, under_present_dir);
            entries.push(Entry {
                rel: rel.clone(),
                state: c.state,
                home: home.cloned(),
                store: store.cloned(),
                dir,
                home_fp: c.home_fp,
                store_fp: c.store_fp,
            });
        }

        let (perms, dir_modes) = self.perm_diffs(&entries);
        Ok(Scan {
            entries,
            absent_dirs,
            empty_dirs,
            perms,
            dir_modes,
            notes,
        })
    }

    /// Compare the permissions of tracked files that are on both sides, and
    /// of the home directories holding tracked paths, with the manifest's
    /// records.
    fn perm_diffs(&self, entries: &[Entry]) -> (Vec<PermDiff>, BTreeMap<Rel, u32>) {
        let recorded = |rel: &Rel| self.manifest.modes.get(rel).copied();
        let diff = |rel: &Rel, is_dir: bool, path: PathBuf, mode: u32| {
            let rec = recorded(rel);
            let d = PermDiff {
                rel: rel.clone(),
                is_dir,
                path,
                home: mode & 0o777,
                recorded: rec,
                record: crate::perms::after_save(mode, rec, false),
                restored: crate::perms::after_restore(mode, rec),
            };
            (d.needs_record() || d.needs_chmod()).then_some(d)
        };
        let mut perms = Vec::new();
        // Directories holding tracked paths, and holding anything at all
        // (a file named on the command line is about to be tracked).
        let mut tracked_dirs = BTreeSet::new();
        let mut all_dirs = BTreeSet::new();
        for e in entries {
            let Some(h) = &e.home else { continue };
            // Tracked: in the store, or new under a tracked directory.
            let tracked = e.store.is_some() || e.dir.is_some();
            if tracked && h.kind == Kind::File && e.store.is_some() {
                perms.extend(diff(&e.rel, false, h.path.clone(), h.mode));
            }
            for d in e.rel.ancestors() {
                if tracked {
                    tracked_dirs.insert(d.clone());
                }
                if !all_dirs.insert(d) {
                    break;
                }
            }
        }
        let mut dir_modes = BTreeMap::new();
        for d in all_dirs {
            let path = self.layout.live(&d);
            // Follow a symlinked directory: its target's permissions count.
            if let Ok(md) = std::fs::metadata(&path)
                && md.is_dir()
            {
                dir_modes.insert(d.clone(), md.mode() & 0o777);
                if tracked_dirs.contains(&d) {
                    perms.extend(diff(&d, true, path, md.mode()));
                }
            }
        }
        perms.sort_by(|a, b| a.rel.cmp(&b.rel));
        (perms, dir_modes)
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

    /// Whether a tracked directory at home holds any file or symlink that is
    /// not ignored, at any depth.
    fn holds_files(&self, dir: &Rel) -> bool {
        let mut walker = WalkDir::new(self.layout.live(dir))
            .follow_links(false)
            .min_depth(1)
            .into_iter();
        while let Some(Ok(entry)) = walker.next() {
            let Ok(rel) = Rel::from_path_under(&self.layout.home, entry.path()) else {
                continue;
            };
            if self.ignore.is_ignored(&rel) {
                if entry.file_type().is_dir() {
                    walker.skip_current_dir();
                }
                continue;
            }
            if !entry.file_type().is_dir() {
                return true;
            }
        }
        false
    }

    /// Walk a tracked directory at home. Returns how many files and symlinks
    /// were seen (ignored ones excluded), in scope or not, in the parts it
    /// walked: subdirectories out of scope are skipped.
    fn walk_home_dir(
        &self,
        dir: &Rel,
        scope: &Scope,
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
            let is_own = self.own.contains(&(meta.dev, meta.ino));
            match meta.kind {
                Kind::Dir => {
                    if is_own || !scope.may_descend(&rel) {
                        walker.skip_current_dir();
                    }
                }
                Kind::File if is_own => {}
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

/// A path's state, with the fingerprints read or cached on the way.
struct Classified {
    state: State,
    home_fp: Option<Fp>,
    store_fp: Option<Fp>,
}

impl Classified {
    fn state(state: State) -> Classified {
        Classified {
            state,
            home_fp: None,
            store_fp: None,
        }
    }
}

impl Scanner<'_> {
    fn classify(
        &self,
        rel: &Rel,
        home: Option<&Meta>,
        store: Option<&Meta>,
        base: Base,
        under_present_dir: bool,
    ) -> Classified {
        // A link at home into the store, as stow makes: copying it over the
        // file it points to would leave a link pointing at itself.
        if let Some(h) = home
            && h.kind == Kind::Symlink
            && self.points_into_store(h)
        {
            return Classified::state(State::Error(
                "a symlink into the store (as stow makes); cubby copies files instead: remove the link and `cubby restore` puts a copy here".into(),
            ));
        }
        match (home, store) {
            (Some(h), Some(s)) if h.kind != s.kind => Classified::state(State::Conflict {
                home: h.kind,
                store: s.kind,
            }),
            (Some(h), Some(s)) => self.compare(rel, h, s, base),
            (Some(h), None) => {
                let stored = self.layout.stored(rel);
                // Never write through a link in the store (it could lead
                // anywhere), and never over a directory there.
                if let Some(link) = fsx::symlink_under(&self.layout.store, &stored) {
                    let link = link.strip_prefix(&self.layout.store).unwrap_or(&link);
                    return Classified::state(State::Error(format!(
                        "{} in the store is a symlink; cubby will not write through it",
                        link.display()
                    )));
                }
                if let Ok(Some(m)) = fsx::lstat(&stored)
                    && m.kind == Kind::Dir
                {
                    return Classified::state(State::Conflict {
                        home: h.kind,
                        store: Kind::Dir,
                    });
                }
                Classified::state(unreadable(h).unwrap_or(State::New {
                    was_stored: base.exists(),
                }))
            }
            (None, Some(s)) => {
                if let Some(e) = unreadable(s) {
                    return Classified::state(e);
                }
                // Changed in the store since it was deleted at home?
                let mut store_fp = None;
                let store_changed = match base {
                    Base::Is(b) => match self.fingerprint(rel, Side::Store, s) {
                        Ok(fp) => {
                            store_fp = Some(fp);
                            fp != b
                        }
                        Err(e) => return Classified::state(State::Error(e)),
                    },
                    _ => false,
                };
                Classified {
                    state: State::Missing {
                        was_here: base.exists(),
                        under_present_dir,
                        store_changed,
                    },
                    home_fp: None,
                    store_fp,
                }
            }
            (None, None) => Classified::state(State::Error("vanished during scan".into())),
        }
    }

    /// Whether a symlink leads into the store, following its target from
    /// where the link is, even when the target does not exist.
    fn points_into_store(&self, link: &Meta) -> bool {
        let store = &self.layout.store;
        if std::fs::canonicalize(&link.path).is_ok_and(|t| t.starts_with(store)) {
            return true;
        }
        let (Some(target), Some(parent)) = (&link.target, link.path.parent()) else {
            return false;
        };
        std::fs::canonicalize(parent)
            .map(|p| crate::paths::normalize(&p.join(target)).starts_with(store))
            .unwrap_or(false)
    }

    /// Compare two paths of the same kind by fingerprint, reading only what
    /// the index has no fingerprint for.
    fn compare(&self, rel: &Rel, h: &Meta, s: &Meta, base: Base) -> Classified {
        let mut home_fp = self.index.cached(rel, Side::Home, h);
        let mut store_fp = self.index.cached(rel, Side::Store, s);
        // Files of different sizes differ; without a baseline to tell which
        // side changed, there is no need to read them.
        if h.kind == Kind::File
            && h.len != s.len
            && !matches!(base, Base::Is(_))
            && (home_fp.is_none() || store_fp.is_none())
        {
            return Classified {
                state: State::Modified(Change::Unknown),
                home_fp,
                store_fp,
            };
        }
        if fsx::same_inode(h, s) && home_fp.is_none() {
            home_fp = store_fp;
        }
        for (fp, side, meta) in [
            (&mut home_fp, Side::Home, h),
            (&mut store_fp, Side::Store, s),
        ] {
            if fp.is_none() {
                match self.fingerprint(rel, side, meta) {
                    Ok(f) => *fp = Some(f),
                    Err(e) => return Classified::state(State::Error(e)),
                }
            }
        }
        let (hf, sf) = (home_fp.expect("read above"), store_fp.expect("read above"));
        let state = if hf == sf {
            State::Same
        } else {
            State::Modified(match base {
                Base::Is(b) if b == hf => Change::Store,
                Base::Is(b) if b == sf => Change::Home,
                Base::Is(_) => Change::Both,
                Base::None | Base::Seen => Change::Unknown,
            })
        };
        Classified {
            state,
            home_fp,
            store_fp,
        }
    }

    /// A path's fingerprint: cached when its metadata has not changed, read
    /// otherwise. Errors are for showing next to the path.
    fn fingerprint(&self, rel: &Rel, side: Side, meta: &Meta) -> Result<Fp, String> {
        if let Some(fp) = self.index.cached(rel, side, meta) {
            return Ok(fp);
        }
        index::fingerprint(meta).map_err(|e| format!("cannot read: {}", e.root_cause()))
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
