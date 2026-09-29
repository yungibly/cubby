//! A record of every run that changed something, with copies of what it
//! overwrote or removed.
//!
//! Each run gets `<state>/runs/<id>/`: `run.toml` lists its actions (what
//! each path held before and after, and the manifest edits it made), and
//! `backup/<path>` holds the previous copies. `cubby history` lists runs,
//! `cubby undo` reverses one, and `cubby backups` finds old copies.
//!
//! Runs are kept for a number of days (`backup_days`, 30 by default), and
//! the newest few of each kind are kept whatever their age, so a burst of
//! saves never pushes out the one restore whose backups matter.
//!
//! cubby 2 wrote `history.log` and `backups/<timestamp>-<operation>/`
//! instead; those are still read, and old backup sets pruned the same way.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::fsx::{self, Meta, Perms};
use crate::manifest::Edit;
use crate::paths::Rel;
use crate::perms;

/// How long runs are kept, by default.
pub const DEFAULT_DAYS: u32 = 30;
/// How many of each kind of run are kept whatever their age.
pub const KEEP_PER_KIND: usize = 5;

/// One run, as written to `run.toml`.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct RunFile {
    pub id: String,
    pub kind: String,
    pub time: String,
    pub store: String,
    pub cubby: String,
    /// For an undo: the run it reverses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undoes: Option<String>,
    #[serde(default, rename = "action", skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Recorded>,
    #[serde(default, rename = "manifest", skip_serializing_if = "Vec::is_empty")]
    pub manifest: Vec<ManifestEdit>,
}

/// One action of a run.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Recorded {
    pub path: String,
    /// `home` or `store`.
    pub side: String,
    /// `create`, `overwrite`, `remove`, or `chmod`.
    pub op: String,
    /// Whether a copy of what was there before is in `backup/`.
    #[serde(default)]
    pub backup: bool,
    /// Fingerprint of what the run left there, for copies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fp: Option<String>,
    /// The index's baseline before the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode_before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode_after: Option<String>,
}

/// A manifest edit, as written to `run.toml`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ManifestEdit {
    /// `add-dir`, `remove-dir`, `add-ignore`, `remove-ignore`, or `mode`.
    pub edit: String,
    /// The path or pattern.
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

impl ManifestEdit {
    pub fn from_edit(e: &Edit) -> ManifestEdit {
        let simple = |edit: &str, value: String| ManifestEdit {
            edit: edit.into(),
            value,
            from: None,
            to: None,
        };
        match e {
            Edit::AddDir(r) => simple("add-dir", r.to_string()),
            Edit::RemoveDir(r) => simple("remove-dir", r.to_string()),
            Edit::AddIgnore(p) => simple("add-ignore", p.clone()),
            Edit::RemoveIgnore(p) => simple("remove-ignore", p.clone()),
            Edit::Mode { rel, from, to } => ManifestEdit {
                edit: "mode".into(),
                value: rel.to_string(),
                from: from.map(perms::show),
                to: to.map(perms::show),
            },
        }
    }

    pub fn to_edit(&self) -> Option<Edit> {
        let mode = |m: &Option<String>| -> Option<Option<u32>> {
            match m {
                None => Some(None),
                Some(t) => u32::from_str_radix(t, 8).ok().map(Some),
            }
        };
        Some(match self.edit.as_str() {
            "add-dir" => Edit::AddDir(Rel::parse(&self.value).ok()?),
            "remove-dir" => Edit::RemoveDir(Rel::parse(&self.value).ok()?),
            "add-ignore" => Edit::AddIgnore(self.value.clone()),
            "remove-ignore" => Edit::RemoveIgnore(self.value.clone()),
            "mode" => Edit::Mode {
                rel: Rel::parse(&self.value).ok()?,
                from: mode(&self.from)?,
                to: mode(&self.to)?,
            },
            _ => return None,
        })
    }
}

/// A run in progress.
pub struct Run {
    dir: PathBuf,
    keep_copies: bool,
    backed_up: usize,
    file: RunFile,
}

impl Run {
    /// Start recording a run of `kind` against `store`. With `keep_copies`
    /// off (`backups = false`), the run is still recorded but nothing is
    /// copied, so overwrites and removals cannot be undone.
    pub fn start(state_dir: &Path, kind: &str, store: &Path, keep_copies: bool) -> Run {
        let now = jiff::Zoned::now();
        let stamp = now.strftime("%Y%m%d-%H%M%S").to_string();
        let root = runs_dir(state_dir);
        let mut id = format!("{stamp}-{kind}");
        let mut n = 1;
        while root.join(&id).exists() {
            n += 1;
            id = format!("{stamp}-{kind}-{n}");
        }
        Run {
            dir: root.join(&id),
            keep_copies,
            backed_up: 0,
            file: RunFile {
                id,
                kind: kind.to_owned(),
                time: now.timestamp().to_string(),
                store: store.display().to_string(),
                cubby: env!("CARGO_PKG_VERSION").to_owned(),
                ..RunFile::default()
            },
        }
    }

    pub fn id(&self) -> &str {
        &self.file.id
    }

    /// This run reverses the run `id`.
    pub fn set_undoes(&mut self, id: &str) {
        self.file.undoes = Some(id.to_owned());
    }

    /// Keep a copy of the file or symlink at `path` before it is
    /// overwritten or removed. Returns whether a copy was made.
    pub fn stash(&mut self, rel: &Rel, path: &Path, meta: &Meta) -> Result<bool> {
        if !self.keep_copies || meta.kind == fsx::Kind::Dir {
            return Ok(false);
        }
        if let Some(root) = self.dir.parent() {
            private_dir(root)?;
        }
        let dest = rel.under(&self.dir.join("backup"));
        fsx::copy_entry(path, meta, &dest, None, &Perms::default())
            .with_context(|| format!("cannot back up {} to {}", path.display(), dest.display()))?;
        self.backed_up += 1;
        Ok(true)
    }

    pub fn record(&mut self, action: Recorded) {
        self.file.actions.push(action);
    }

    pub fn record_manifest(&mut self, edits: &[Edit]) {
        self.file
            .manifest
            .extend(edits.iter().map(ManifestEdit::from_edit));
    }

    pub fn backed_up(&self) -> usize {
        self.backed_up
    }

    /// Write `run.toml`, unless the run did nothing.
    pub fn finish(self) -> Result<()> {
        if self.file.actions.is_empty() && self.file.manifest.is_empty() {
            return Ok(());
        }
        let text = toml::to_string(&self.file).context("cannot describe the run")?;
        if let Some(root) = self.dir.parent() {
            private_dir(root)?;
        }
        let path = self.dir.join("run.toml");
        fsx::write_atomic(&path, text.as_bytes())
            .with_context(|| format!("cannot write {}", path.display()))
    }
}

/// A finished run.
#[derive(Clone, Debug)]
pub struct Info {
    pub dir: PathBuf,
    pub file: RunFile,
    pub time: jiff::Timestamp,
}

impl Info {
    /// Where the copy of `rel` is kept, if this run kept one.
    pub fn backup_of(&self, rel: &Rel) -> Option<PathBuf> {
        let path = rel.under(&self.dir.join("backup"));
        fsx::lstat(&path).ok().flatten().map(|_| path)
    }

    /// Paths this run kept copies of, and their total size.
    pub fn copies(&self) -> (usize, u64) {
        let root = self.dir.join("backup");
        walkdir::WalkDir::new(&root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| !e.file_type().is_dir())
            .fold((0, 0), |(n, size), e| {
                (n + 1, size + e.metadata().map_or(0, |m| m.len()))
            })
    }
}

fn runs_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("runs")
}

/// Every recorded run, oldest first. Runs whose record cannot be read are
/// left out.
pub fn list(state_dir: &Path) -> Vec<Info> {
    let Ok(entries) = std::fs::read_dir(runs_dir(state_dir)) else {
        return Vec::new();
    };
    let mut runs: Vec<Info> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let dir = e.path();
            let text = std::fs::read_to_string(dir.join("run.toml")).ok()?;
            let file: RunFile = toml::from_str(&text).ok()?;
            let time = file.time.parse().ok()?;
            Some(Info { dir, file, time })
        })
        .collect();
    runs.sort_by(|a, b| a.time.cmp(&b.time).then(a.file.id.cmp(&b.file.id)));
    runs
}

/// The ids of runs that stand reversed: an undo counts only while it has
/// not been undone itself.
pub fn undone(runs: &[Info]) -> BTreeSet<String> {
    let mut undone = BTreeSet::new();
    for r in runs.iter().rev() {
        if undone.contains(&r.file.id) {
            continue;
        }
        if let Some(of) = &r.file.undoes {
            undone.insert(of.clone());
        }
    }
    undone
}

/// Make `dir` and keep it to its owner: it holds copies of dotfiles.
fn private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let mode = std::fs::metadata(dir)?.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        fsx::chmod(dir, mode & 0o700)?;
    }
    Ok(())
}

/// A backup set cubby 2 left in `<state>/backups/`.
pub struct Legacy {
    pub dir: PathBuf,
    pub name: String,
    pub time: Option<jiff::civil::DateTime>,
}

impl Legacy {
    /// The operation a set was made by: `save` in `20260101-120000-save-2`.
    fn kind(&self) -> &str {
        let rest = self.name.get(16..).unwrap_or("");
        match rest.rsplit_once('-') {
            Some((kind, n)) if n.chars().all(|c| c.is_ascii_digit()) => kind,
            _ => rest,
        }
    }
}

/// cubby 2's backup sets, oldest first.
pub fn legacy_sets(state_dir: &Path) -> Vec<Legacy> {
    let Ok(entries) = std::fs::read_dir(state_dir.join("backups")) else {
        return Vec::new();
    };
    let mut sets: Vec<Legacy> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let time = name
                .get(..15)
                .and_then(|t| jiff::civil::DateTime::strptime("%Y%m%d-%H%M%S", t).ok());
            Legacy {
                dir: e.path(),
                name,
                time,
            }
        })
        .collect();
    sets.sort_by(|a, b| a.name.cmp(&b.name));
    sets
}

/// Remove runs, and cubby 2 backup sets, older than `days`, keeping the
/// newest [`KEEP_PER_KIND`] of each kind whatever their age. Returns how
/// many were removed.
pub fn prune(state_dir: &Path, days: u32) -> Result<usize> {
    let now = jiff::Timestamp::now();
    // More days than time goes back means keeping everything.
    let cutoff = now
        .checked_sub(jiff::SignedDuration::from_hours(24 * i64::from(days)))
        .unwrap_or(jiff::Timestamp::MIN);
    let mut removed = 0;
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for run in list(state_dir).iter().rev() {
        let n = seen.entry(run.file.kind.clone()).or_default();
        *n += 1;
        if run.time < cutoff && *n > KEEP_PER_KIND {
            std::fs::remove_dir_all(&run.dir)
                .with_context(|| format!("cannot remove {}", run.dir.display()))?;
            removed += 1;
        }
    }
    let tz = jiff::tz::TimeZone::system();
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    let legacy = legacy_sets(state_dir);
    for set in legacy.iter().rev() {
        let n = seen.entry(set.kind()).or_default();
        *n += 1;
        let old = set
            .time
            .and_then(|t| t.to_zoned(tz.clone()).ok())
            .is_some_and(|t| t.timestamp() < cutoff);
        if old && *n > KEEP_PER_KIND {
            std::fs::remove_dir_all(&set.dir)
                .with_context(|| format!("cannot remove {}", set.dir.display()))?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Paths whose last operation in cubby 2's `history.log` copied them one
/// way or the other: they were in sync on this machine at some point.
pub fn legacy_synced(state_dir: &Path) -> Vec<Rel> {
    let Ok(text) = std::fs::read_to_string(state_dir.join("history.log")) else {
        return Vec::new();
    };
    let mut last: BTreeMap<&str, &str> = BTreeMap::new();
    for line in text.lines() {
        let mut parts = line.splitn(3, '\t');
        if let (Some(_), Some(op), Some(rel)) = (parts.next(), parts.next(), parts.next()) {
            last.insert(rel, op);
        }
    }
    last.into_iter()
        .filter(|(_, op)| *op == "save" || *op == "restore")
        .filter_map(|(rel, _)| Rel::parse(rel).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::sandbox;

    fn rel(s: &str) -> Rel {
        Rel::parse(s).unwrap()
    }

    #[test]
    fn a_run_keeps_copies_and_a_record() {
        let sb = sandbox();
        let state = sb.path().join("state");
        let file = sb.path().join("file");
        std::fs::write(&file, b"contents").unwrap();
        let meta = fsx::lstat(&file).unwrap().unwrap();

        let mut run = Run::start(&state, "save", Path::new("/store"), true);
        assert!(run.stash(&rel(".config/x"), &file, &meta).unwrap());
        run.record(Recorded {
            path: ".config/x".into(),
            side: "store".into(),
            op: "overwrite".into(),
            backup: true,
            ..Recorded::default()
        });
        run.record_manifest(&[Edit::Mode {
            rel: rel(".netrc"),
            from: None,
            to: Some(0o600),
        }]);
        let id = run.id().to_owned();
        run.finish().unwrap();

        let runs = list(&state);
        assert_eq!(runs.len(), 1);
        let r = &runs[0];
        assert_eq!(r.file.id, id);
        assert_eq!(r.file.actions[0].op, "overwrite");
        assert_eq!(
            r.file.manifest[0].to_edit(),
            Some(Edit::Mode {
                rel: rel(".netrc"),
                from: None,
                to: Some(0o600)
            })
        );
        assert_eq!(
            std::fs::read(r.backup_of(&rel(".config/x")).unwrap()).unwrap(),
            b"contents"
        );
        assert_eq!(r.copies(), (1, 8));
        // A second run in the same second gets its own directory.
        let second = Run::start(&state, "save", Path::new("/store"), true);
        assert_ne!(second.id(), id);
        // A run that did nothing leaves nothing behind.
        second.finish().unwrap();
        assert_eq!(list(&state).len(), 1);
    }

    #[test]
    fn pruning_keeps_recent_runs_and_the_newest_of_each_kind() {
        let sb = sandbox();
        let state = sb.path().join("state");
        let old = |i: usize, kind: &str| {
            let id = format!("2020010{i}-000000-{kind}");
            let file = RunFile {
                id: id.clone(),
                kind: kind.into(),
                time: format!("2020-01-0{i}T00:00:00Z"),
                actions: vec![Recorded::default()],
                ..RunFile::default()
            };
            let dir = runs_dir(&state).join(&id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("run.toml"), toml::to_string(&file).unwrap()).unwrap();
        };
        for i in 1..=7 {
            old(i, "save");
        }
        old(1, "restore");
        let mut run = Run::start(&state, "save", Path::new("/s"), true);
        run.record(Recorded::default());
        run.finish().unwrap();
        for i in 1..=7 {
            std::fs::create_dir_all(state.join(format!("backups/2020010{i}-000000-save"))).unwrap();
        }
        std::fs::create_dir_all(state.join("backups/20200101-000000-restore-2")).unwrap();

        // "Forever" keeps everything.
        assert_eq!(prune(&state, u32::MAX).unwrap(), 0);
        // Of eight saves, the newest five stay; the lone old restore stays,
        // and the same goes for cubby 2's sets.
        assert_eq!(prune(&state, 30).unwrap(), 3 + 2);
        let kinds: Vec<String> = list(&state).into_iter().map(|r| r.file.kind).collect();
        assert_eq!(kinds.iter().filter(|k| *k == "save").count(), 5);
        assert_eq!(kinds.iter().filter(|k| *k == "restore").count(), 1);
        let legacy: Vec<String> = legacy_sets(&state).into_iter().map(|s| s.name).collect();
        assert_eq!(legacy.len(), KEEP_PER_KIND + 1, "{legacy:?}");
        assert!(legacy.contains(&"20200101-000000-restore-2".to_owned()));
    }

    #[test]
    fn legacy_history_counts_saved_and_restored_paths() {
        let sb = sandbox();
        std::fs::write(
            sb.path().join("history.log"),
            "t\tsave\t.zshrc\nt\tsave\t.vimrc\nt\tuntrack\t.vimrc\nt\trestore\t.gitconfig\ngarbage\n",
        )
        .unwrap();
        assert_eq!(
            legacy_synced(sb.path()),
            vec![rel(".gitconfig"), rel(".zshrc")]
        );
    }
}
