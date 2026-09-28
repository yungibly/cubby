//! Turn a scan into a list of actions, and carry them out.
//!
//! `save` copies home → store and, under tracked directories, removes store
//! files that were deleted at home; it also records the permissions of
//! private files in the manifest. `restore` copies store → home, applies
//! recorded permissions, and never removes anything. `untrack` removes
//! files from the store. All of them back up whatever they overwrite or
//! remove.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};

use crate::fsx::{self, Kind, Perms};
use crate::index::{self, Index};
pub use crate::paths::Side;
use crate::paths::{Layout, Rel};
use crate::perms;
use crate::runs::{Recorded, Run};
use crate::scan::{Change, Entry, Scan, State};

/// Which way a save or restore copies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// home → store
    Save,
    /// store → home
    Restore,
}

impl Direction {
    /// The side this direction writes.
    pub fn writes(self) -> Side {
        match self {
            Direction::Save => Side::Store,
            Direction::Restore => Side::Home,
        }
    }
}

/// What a run of cubby is doing, for messages, backups, and history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunKind {
    Save,
    Restore,
    Sync,
    Untrack,
    Undo,
}

impl RunKind {
    pub fn verb(self) -> &'static str {
        match self {
            RunKind::Save => "save",
            RunKind::Restore => "restore",
            RunKind::Sync => "sync",
            RunKind::Untrack => "untrack",
            RunKind::Undo => "undo",
        }
    }

    /// "saved", as in "nothing saved".
    pub fn past(self) -> &'static str {
        match self {
            RunKind::Save => "saved",
            RunKind::Restore => "restored",
            RunKind::Sync => "synced",
            RunKind::Untrack => "removed",
            RunKind::Undo => "undid",
        }
    }

    /// "saved 3 changes", for the line after a run.
    pub fn summary(self, done: usize) -> String {
        match self {
            RunKind::Untrack => format!(
                "removed {} from the store",
                crate::ui::plural(done, "file", "files")
            ),
            _ => format!(
                "{} {}",
                self.past(),
                crate::ui::plural(done, "change", "changes")
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// Create the destination.
    Create,
    /// Replace the destination.
    Overwrite,
    /// Remove the destination.
    Remove,
    /// Change the destination's permissions.
    Chmod,
}

#[derive(Clone, Debug)]
pub struct Action {
    pub rel: Rel,
    pub op: Op,
    /// The side that gets written.
    pub side: Side,
    /// Short explanation shown next to the path.
    pub note: String,
    /// Where to copy from (absent for removals and permission changes).
    pub src: Option<PathBuf>,
    /// The path that gets written, removed, or changed.
    pub dst: PathBuf,
    /// Bytes copied (zero unless copying).
    pub len: u64,
    /// For copies: permissions to take away from the copy and from any
    /// directories created for it.
    pub perms: Perms,
    /// For permission changes: the new permission bits.
    pub mode: Option<u32>,
}

/// A change to the permissions the manifest records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeRecord {
    pub rel: Rel,
    pub is_dir: bool,
    pub from: Option<u32>,
    pub to: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Skip {
    /// In the store but not at home, and not a deletion to mirror: new in
    /// the store, or (`was_here`) deleted at home outside a tracked
    /// directory.
    MissingAtHome {
        was_here: bool,
    },
    /// At home but not in the store.
    NewAtHome,
    /// Changed on this side since the last sync; copying over it would undo
    /// that change. Needs `--force`.
    ChangedThere(Side),
    /// Deleted from the store since the last sync; saving would add it
    /// back. Needs `--force`.
    DeletedFromStore,
    Conflict(String),
    Error(String),
}

#[derive(Clone, Debug)]
pub struct Skipped {
    pub rel: Rel,
    pub why: Skip,
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub kind: RunKind,
    pub actions: Vec<Action>,
    pub skipped: Vec<Skipped>,
    /// Permission records to write to the manifest.
    pub records: Vec<ModeRecord>,
    /// For an undo: the run it reverses.
    pub undoes: Option<String>,
}

impl Plan {
    pub fn new(kind: RunKind) -> Plan {
        Plan {
            kind,
            actions: Vec::new(),
            skipped: Vec::new(),
            records: Vec::new(),
            undoes: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.actions.is_empty() && self.records.is_empty()
    }

    pub fn count(&self, op: Op) -> usize {
        self.actions.iter().filter(|a| a.op == op).count()
    }

    /// Size of everything the plan copies; for previews.
    pub fn bytes_to_copy(&self) -> u64 {
        self.actions.iter().map(|a| a.len).sum()
    }

    /// How many paths were skipped because of a conflict or an error: the
    /// run cannot do everything it was asked to, so it exits with 1.
    pub fn troubled(&self) -> usize {
        self.skipped
            .iter()
            .filter(|s| matches!(s.why, Skip::Conflict(_) | Skip::Error(_)))
            .count()
    }

    /// Records with no action of their own to show them (the note of a
    /// file's copy mentions its record): each is a change of its own.
    pub fn standalone_records(&self) -> impl Iterator<Item = &ModeRecord> {
        self.records
            .iter()
            .filter(|r| !self.actions.iter().any(|a| a.rel == r.rel))
    }

    /// Paths whose recorded permissions this plan sets, not counting the
    /// records dropped along with a file removed from the store.
    pub fn records_set(&self) -> usize {
        self.records.iter().filter(|r| r.to.is_some()).count()
    }
}

/// Plan a save or a restore of everything in `scan`. `modes` are the
/// permissions the manifest records.
pub fn plan(
    scan: &Scan,
    layout: &Layout,
    modes: &BTreeMap<Rel, u32>,
    direction: Direction,
    force: bool,
) -> Plan {
    let kind = match direction {
        Direction::Save => RunKind::Save,
        Direction::Restore => RunKind::Restore,
    };
    let mut plan = Plan::new(kind);
    for e in &scan.entries {
        plan_entry(&mut plan, e, scan, layout, modes, direction, force);
    }
    match direction {
        Direction::Save => plan_records(&mut plan, scan, modes, force),
        Direction::Restore => plan_chmods(&mut plan, scan),
    }
    plan
}

/// Plan a sync: each path is copied the way it changed since the last
/// sync. Paths changed on both sides, and paths that differ with no record
/// of the last sync, are left for the person to decide.
pub fn sync_plan(scan: &Scan, layout: &Layout, modes: &BTreeMap<Rel, u32>) -> Plan {
    let mut plan = Plan::new(RunKind::Sync);
    for e in &scan.entries {
        let skip = |why: Skip| Skipped {
            rel: e.rel.clone(),
            why,
        };
        let direction = match &e.state {
            State::Same => continue,
            State::Modified(Change::Home) => Direction::Save,
            State::Modified(Change::Store) => Direction::Restore,
            State::Modified(Change::Unknown) => {
                plan.skipped.push(skip(Skip::Conflict(
                    "differs, with no record of the last sync".into(),
                )));
                continue;
            }
            State::New { .. } if e.dir.is_none() => continue,
            State::New { .. } => Direction::Save,
            State::Missing {
                was_here: false, ..
            } => Direction::Restore,
            State::Missing {
                under_present_dir: true,
                ..
            } => Direction::Save,
            State::Missing { .. } => {
                plan.skipped
                    .push(skip(Skip::MissingAtHome { was_here: true }));
                continue;
            }
            // Both sides changed, conflicts of kind, and errors are skipped
            // the same way in either direction.
            State::Modified(Change::Both) | State::Conflict { .. } | State::Error(_) => {
                Direction::Save
            }
        };
        plan_entry(&mut plan, e, scan, layout, modes, direction, false);
    }
    plan_records(&mut plan, scan, modes, false);
    plan_chmods(&mut plan, scan);
    plan
}

/// Plan what copying one path in `direction` takes, or why it is skipped.
fn plan_entry(
    plan: &mut Plan,
    e: &Entry,
    scan: &Scan,
    layout: &Layout,
    modes: &BTreeMap<Rel, u32>,
    direction: Direction,
    force: bool,
) {
    let skip = |why: Skip| Skipped {
        rel: e.rel.clone(),
        why,
    };
    // Real on-disk paths where they exist, the mirrored path otherwise.
    let home_path = e
        .home
        .as_ref()
        .map(|m| m.path.clone())
        .unwrap_or_else(|| layout.live(&e.rel));
    let store_path = e
        .store
        .as_ref()
        .map(|m| m.path.clone())
        .unwrap_or_else(|| layout.stored(&e.rel));
    let copy = |op: Op, mut note: String| {
        let (src, src_meta, dst) = match direction {
            Direction::Save => (home_path.clone(), e.home.as_ref(), store_path.clone()),
            Direction::Restore => (store_path.clone(), e.store.as_ref(), home_path.clone()),
        };
        if let Some(m) = src_meta
            && let Some(target) = &m.target
        {
            note.push_str(&format!(", link → {}", target.display()));
            // A relative target only means something where the link
            // lives, so this is only known for links at home.
            if direction == Direction::Save && m.points_to_dir {
                note.push_str(" (a directory; the link is saved, not its contents)");
            }
        }
        Action {
            rel: e.rel.clone(),
            op,
            side: direction.writes(),
            note,
            src: Some(src),
            dst,
            len: src_meta.map_or(0, |m| m.len),
            perms: copy_perms(e, scan, layout, modes, direction),
            mode: None,
        }
    };
    let remove = |note: &str| Action {
        rel: e.rel.clone(),
        op: Op::Remove,
        side: Side::Store,
        note: note.to_owned(),
        src: None,
        dst: store_path.clone(),
        len: 0,
        perms: Perms::default(),
        mode: None,
    };
    // The side this run would copy over, when that side changed.
    let there = match direction {
        Direction::Save => (Change::Store, "changed in the store"),
        Direction::Restore => (Change::Home, "changed at home"),
    };
    match (&e.state, direction) {
        (State::Same, _) => {}
        (State::Modified(Change::Both), _) if !force => plan.skipped.push(skip(Skip::Conflict(
            "changed at home and in the store".into(),
        ))),
        (State::Modified(change), _) if *change == there.0 && !force => plan
            .skipped
            .push(skip(Skip::ChangedThere(direction.writes()))),
        (State::Modified(change), _) => {
            let note = match change {
                Change::Home => "changed at home".to_owned(),
                Change::Store => "changed in the store".to_owned(),
                Change::Unknown => "modified".to_owned(),
                Change::Both => "changed at home and in the store".to_owned(),
            };
            let note = if *change == there.0 || *change == Change::Both {
                format!("{note}; replacing that copy")
            } else {
                note
            };
            plan.actions.push(copy(Op::Overwrite, note));
        }
        (State::New { was_stored: false }, Direction::Save) => {
            plan.actions.push(copy(Op::Create, "new".into()))
        }
        (State::New { was_stored: true }, Direction::Save) if force => plan.actions.push(copy(
            Op::Create,
            "deleted from the store since the last sync; adding it back".into(),
        )),
        (State::New { was_stored: true }, Direction::Save) => {
            plan.skipped.push(skip(Skip::DeletedFromStore))
        }
        (State::New { .. }, Direction::Restore) => plan.skipped.push(skip(Skip::NewAtHome)),
        (
            State::Missing {
                was_here: true,
                under_present_dir: true,
                store_changed,
            },
            Direction::Save,
        ) => match (store_changed, force) {
            (false, _) => plan.actions.push(remove("deleted at home")),
            (true, true) => plan
                .actions
                .push(remove("deleted at home; removing the store's newer copy")),
            (true, false) => plan.skipped.push(skip(Skip::Conflict(
                "deleted at home, changed in the store".into(),
            ))),
        },
        (State::Missing { was_here, .. }, Direction::Save) => {
            plan.skipped.push(skip(Skip::MissingAtHome {
                was_here: *was_here,
            }))
        }
        (State::Missing { was_here, .. }, Direction::Restore) => {
            let note = if *was_here {
                "deleted at home"
            } else {
                "new in the store"
            };
            plan.actions.push(copy(Op::Create, note.into()))
        }
        (State::Conflict { home, store }, dir) => {
            let text = format!(
                "home has {}, store has {}",
                home.describe(),
                store.describe()
            );
            let replaceable = !matches!(home, Kind::Dir) && !matches!(store, Kind::Dir);
            if force && replaceable {
                let note = match dir {
                    Direction::Save => format!("{text}; replacing the store copy"),
                    Direction::Restore => format!("{text}; replacing the home copy"),
                };
                plan.actions.push(copy(Op::Overwrite, note));
            } else {
                plan.skipped.push(skip(Skip::Conflict(text)));
            }
        }
        (State::Error(msg), _) => plan.skipped.push(skip(Skip::Error(msg.clone()))),
    }
}

/// How a copy of `e` treats permissions. Saving keeps a private file (and
/// the private directories around it) private in the store too; restoring
/// applies what the manifest records.
fn copy_perms(
    e: &Entry,
    scan: &Scan,
    layout: &Layout,
    modes: &BTreeMap<Rel, u32>,
    direction: Direction,
) -> Perms {
    match direction {
        Direction::Save => Perms {
            restrict: e
                .home
                .as_ref()
                .filter(|m| m.kind == Kind::File && perms::is_private(m.mode))
                .map(|m| m.mode & 0o777),
            dirs: e
                .rel
                .ancestors()
                .filter_map(|d| {
                    let mode = *scan.dir_modes.get(&d)?;
                    perms::is_private(mode).then(|| (layout.stored(&d), mode))
                })
                .collect(),
        },
        Direction::Restore => Perms {
            restrict: modes.get(&e.rel).copied(),
            dirs: e
                .rel
                .ancestors()
                .filter_map(|d| Some((layout.live(&d), *modes.get(&d)?)))
                .collect(),
        },
    }
}

/// The permission records a save writes: private modes of files on both
/// sides, of files it copies, and of the directories holding them; and
/// records dropped for files it removes. A file's record is also mentioned
/// in the note of its copy.
fn plan_records(plan: &mut Plan, scan: &Scan, modes: &BTreeMap<Rel, u32>, force: bool) {
    let mut records: BTreeMap<Rel, ModeRecord> = BTreeMap::new();
    let mut add = |rel: &Rel, is_dir: bool, from: Option<u32>, to: Option<u32>| {
        if to != from {
            records.entry(rel.clone()).or_insert(ModeRecord {
                rel: rel.clone(),
                is_dir,
                from,
                to,
            });
        }
    };
    for p in &scan.perms {
        add(
            &p.rel,
            p.is_dir,
            p.recorded,
            perms::after_save(p.home, p.recorded, force),
        );
    }
    for a in plan.actions.iter().filter(|a| a.side == Side::Store) {
        let from = modes.get(&a.rel).copied();
        if a.op == Op::Remove {
            add(&a.rel, false, from, None);
            continue;
        }
        if let Ok(i) = scan.entries.binary_search_by(|e| e.rel.cmp(&a.rel))
            && let Some(h) = &scan.entries[i].home
        {
            let to = match h.kind {
                Kind::File => perms::after_save(h.mode, from, force),
                _ => None,
            };
            add(&a.rel, false, from, to);
        }
        for d in a.rel.ancestors() {
            if let Some(mode) = scan.dir_modes.get(&d) {
                let from = modes.get(&d).copied();
                add(&d, true, from, perms::after_save(*mode, from, force));
            }
        }
    }
    for a in plan.actions.iter_mut().filter(|a| a.side == Side::Store) {
        if a.op != Op::Remove
            && let Some(r) = records.get(&a.rel)
        {
            match r.to {
                Some(to) => a
                    .note
                    .push_str(&format!(", permissions {}", perms::show(to))),
                None => a.note.push_str(", forget recorded permissions"),
            }
        }
    }
    plan.records = records.into_values().collect();
}

/// Permission changes a restore makes at home: whatever is looser than the
/// manifest records, unless a copy of that file already takes care of it.
fn plan_chmods(plan: &mut Plan, scan: &Scan) {
    let copied: Vec<Rel> = plan
        .actions
        .iter()
        .filter(|a| a.side == Side::Home)
        .map(|a| a.rel.clone())
        .collect();
    for p in scan.perms.iter().filter(|p| p.needs_chmod()) {
        if !p.is_dir && copied.contains(&p.rel) {
            continue;
        }
        plan.actions.push(Action {
            rel: p.rel.clone(),
            op: Op::Chmod,
            side: Side::Home,
            note: format!(
                "permissions {} → {}",
                perms::show(p.home),
                perms::show(p.restored)
            ),
            src: None,
            dst: p.path.clone(),
            len: 0,
            perms: Perms::default(),
            mode: Some(p.restored),
        });
    }
}

/// Plan removing every file of `scan` from the store.
pub fn untrack_plan(scan: &Scan) -> Plan {
    removal_plan(
        scan.entries
            .iter()
            .filter_map(|e| Some((e.rel.clone(), e.store.as_ref()?.path.clone()))),
    )
}

/// Plan removing these store files, given as a path and where it is.
pub fn removal_plan(files: impl IntoIterator<Item = (Rel, PathBuf)>) -> Plan {
    let mut plan = Plan::new(RunKind::Untrack);
    for (rel, dst) in files {
        plan.actions.push(Action {
            rel,
            op: Op::Remove,
            side: Side::Store,
            note: String::new(),
            src: None,
            dst,
            len: 0,
            perms: Perms::default(),
            mode: None,
        });
    }
    plan
}

pub struct Outcome {
    pub done: usize,
    pub failed: Vec<(Action, String)>,
}

/// Carry out a plan, recording each action in `run` (with a copy of
/// whatever it overwrites or removes) and keeping the index's baselines up
/// to date. `report` is called after each action with the result.
pub fn apply(
    plan: &Plan,
    layout: &Layout,
    run: &mut Run,
    index: &mut Index,
    mut report: impl FnMut(&Action, Result<(), &str>),
) -> Outcome {
    let mut done = 0;
    let mut failed = Vec::new();
    for action in &plan.actions {
        let base = index.base(&action.rel);
        match perform(action, layout, run) {
            Ok(performed) => {
                done += 1;
                let mut fp = None;
                match action.op {
                    // Both sides now hold what was copied.
                    Op::Create | Op::Overwrite => {
                        let now = fsx::lstat(&action.dst).ok().flatten();
                        match now.map(|m| index::fingerprint(&m)) {
                            Some(Ok(f)) => {
                                index.synced(&action.rel, f);
                                fp = Some(f.hex());
                            }
                            _ => index.forget(&action.rel),
                        }
                    }
                    Op::Remove => index.forget(&action.rel),
                    Op::Chmod => {}
                }
                run.record(Recorded {
                    path: action.rel.as_str().to_owned(),
                    side: match action.side {
                        Side::Home => "home".into(),
                        Side::Store => "store".into(),
                    },
                    op: match action.op {
                        Op::Create => "create".into(),
                        Op::Overwrite => "overwrite".into(),
                        Op::Remove => "remove".into(),
                        Op::Chmod => "chmod".into(),
                    },
                    backup: performed.backed_up,
                    fp,
                    base: Some(base.to_text()),
                    mode_before: performed.mode_before.map(perms::show),
                    mode_after: action.mode.map(perms::show),
                });
                report(action, Ok(()));
            }
            Err(e) => {
                let msg = format!("{e:#}");
                report(action, Err(&msg));
                failed.push((action.clone(), msg));
            }
        }
    }
    Outcome { done, failed }
}

/// What carrying out an action involved, for its record.
struct Performed {
    backed_up: bool,
    mode_before: Option<u32>,
}

fn perform(action: &Action, layout: &Layout, run: &mut Run) -> Result<Performed> {
    let rel = &action.rel;
    let mut done = Performed {
        backed_up: false,
        mode_before: None,
    };
    match action.op {
        Op::Remove => {
            if let Some(meta) = fsx::lstat(&action.dst)? {
                done.backed_up = run.stash(rel, &action.dst, &meta)?;
            }
            // Directories left empty are pruned in the store, which cubby
            // owns, but never at home.
            let prune_to = match action.side {
                Side::Store => Some(layout.store.as_path()),
                Side::Home => None,
            };
            fsx::remove_entry(&action.dst, prune_to)?;
        }
        Op::Chmod => {
            let mode = action.mode.ok_or_else(|| anyhow!("no mode for {rel}"))?;
            let meta = std::fs::metadata(&action.dst)
                .with_context(|| format!("cannot read {}", action.dst.display()))?;
            done.mode_before =
                Some(std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o7777);
            fsx::chmod(&action.dst, mode)?;
        }
        Op::Create | Op::Overwrite => {
            let src = action
                .src
                .as_ref()
                .ok_or_else(|| anyhow!("no source for {rel}"))?;
            // Look again rather than trusting the scan: things change.
            let src_meta = fsx::lstat(src)?.ok_or_else(|| anyhow!("{} vanished", src.display()))?;
            let dst_meta = fsx::lstat(&action.dst)?;
            if let Some(d) = &dst_meta {
                done.backed_up = run
                    .stash(rel, &action.dst, d)
                    .with_context(|| format!("cannot back up {}", action.dst.display()))?;
            }
            fsx::copy_entry(
                src,
                &src_meta,
                &action.dst,
                dst_meta.as_ref(),
                &action.perms,
            )?;
        }
    }
    Ok(done)
}
