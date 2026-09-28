//! Turn a scan into a list of actions, and carry them out.
//!
//! `save` copies home → store and, under tracked directories, removes store
//! files that were deleted at home. `restore` copies store → home and never
//! removes anything. `untrack` removes files from the store. All of them
//! back up whatever they overwrite or remove.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};

use crate::backup::Backup;
use crate::fsx;
use crate::history::History;
use crate::paths::{Layout, Rel};
use crate::scan::{Newer, Scan, State};

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

/// One side of the mirror.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Home,
    Store,
}

/// What a run of cubby is doing, for messages, backups, and history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunKind {
    Save,
    Restore,
    Untrack,
}

impl RunKind {
    pub fn verb(self) -> &'static str {
        match self {
            RunKind::Save => "save",
            RunKind::Restore => "restore",
            RunKind::Untrack => "untrack",
        }
    }

    /// "saved 3 changes", for the line after a run.
    pub fn summary(self, done: usize) -> String {
        match self {
            RunKind::Save => format!("saved {}", crate::ui::plural(done, "change", "changes")),
            RunKind::Restore => {
                format!("restored {}", crate::ui::plural(done, "change", "changes"))
            }
            RunKind::Untrack => format!(
                "removed {} from the store",
                crate::ui::plural(done, "file", "files")
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
}

#[derive(Clone, Debug)]
pub struct Action {
    pub rel: Rel,
    pub op: Op,
    /// The side that gets written.
    pub side: Side,
    /// Short explanation shown next to the path.
    pub note: String,
    /// Where to copy from (absent for removals).
    pub src: Option<PathBuf>,
    /// The path that gets written or removed.
    pub dst: PathBuf,
    /// Bytes copied (zero for removals).
    pub len: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Skip {
    /// In the store but not at home, and not under a tracked directory
    /// that exists at home (so it was not "deleted", it is just absent).
    MissingAtHome,
    /// At home but not in the store.
    NewAtHome,
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
}

impl Plan {
    pub fn new(kind: RunKind) -> Plan {
        Plan {
            kind,
            actions: Vec::new(),
            skipped: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    pub fn count(&self, op: Op) -> usize {
        self.actions.iter().filter(|a| a.op == op).count()
    }

    /// Size of everything the plan copies; for previews.
    pub fn bytes_to_copy(&self) -> u64 {
        self.actions.iter().map(|a| a.len).sum()
    }
}

/// Plan a save or a restore of everything in `scan`.
pub fn plan(scan: &Scan, layout: &Layout, direction: Direction, force: bool) -> Plan {
    let kind = match direction {
        Direction::Save => RunKind::Save,
        Direction::Restore => RunKind::Restore,
    };
    let mut plan = Plan::new(kind);
    for e in &scan.entries {
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
            }
        };
        match (&e.state, direction) {
            (State::Same, _) => {}
            (State::Modified(newer), dir) => {
                let warn = match (newer, dir) {
                    (Newer::Store, Direction::Save) => ", store copy is newer",
                    (Newer::Home, Direction::Restore) => ", home copy is newer",
                    _ => "",
                };
                plan.actions
                    .push(copy(Op::Overwrite, format!("modified{warn}")));
            }
            (State::New, Direction::Save) => plan.actions.push(copy(Op::Create, "new".into())),
            (State::New, Direction::Restore) => plan.skipped.push(skip(Skip::NewAtHome)),
            (State::Missing { deleted: true }, Direction::Save) => plan.actions.push(Action {
                rel: e.rel.clone(),
                op: Op::Remove,
                side: Side::Store,
                note: "deleted at home".into(),
                src: None,
                dst: store_path.clone(),
                len: 0,
            }),
            (State::Missing { deleted: false }, Direction::Save) => {
                plan.skipped.push(skip(Skip::MissingAtHome))
            }
            (State::Missing { .. }, Direction::Restore) => plan
                .actions
                .push(copy(Op::Create, "missing at home".into())),
            (State::Conflict { home, store }, dir) => {
                let text = format!(
                    "home has {}, store has {}",
                    home.describe(),
                    store.describe()
                );
                let replaceable =
                    !matches!(home, fsx::Kind::Dir) && !matches!(store, fsx::Kind::Dir);
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
    plan
}

/// Plan removing every file of `scan` from the store.
pub fn untrack_plan(scan: &Scan) -> Plan {
    let mut plan = Plan::new(RunKind::Untrack);
    for e in &scan.entries {
        if let Some(m) = &e.store {
            plan.actions.push(Action {
                rel: e.rel.clone(),
                op: Op::Remove,
                side: Side::Store,
                note: String::new(),
                src: None,
                dst: m.path.clone(),
                len: 0,
            });
        }
    }
    plan
}

pub struct Outcome {
    pub done: usize,
    pub failed: Vec<(Action, String)>,
    pub backup_dir: Option<PathBuf>,
    pub backed_up: usize,
}

/// Carry out a plan. `report` is called after each action with the result.
pub fn apply(
    plan: &Plan,
    layout: &Layout,
    mut backup: Option<Backup>,
    history: &History,
    mut report: impl FnMut(&Action, Result<(), &str>),
) -> Result<Outcome> {
    let mut done = 0;
    let mut failed = Vec::new();
    for action in &plan.actions {
        let result = perform(action, layout, backup.as_mut());
        match result {
            Ok(()) => {
                done += 1;
                history.record(op_name(action, plan.kind), &action.rel)?;
                report(action, Ok(()));
            }
            Err(e) => {
                let msg = format!("{e:#}");
                report(action, Err(&msg));
                failed.push((action.clone(), msg));
            }
        }
    }
    let (backup_dir, backed_up) = match backup {
        Some(b) if b.count() > 0 => (Some(b.dir().to_path_buf()), b.count()),
        _ => (None, 0),
    };
    Ok(Outcome {
        done,
        failed,
        backup_dir,
        backed_up,
    })
}

fn op_name(action: &Action, kind: RunKind) -> &'static str {
    match (action.op, action.side, kind) {
        (Op::Remove, _, RunKind::Untrack) => "untrack",
        (Op::Remove, _, _) => "remove",
        (_, Side::Store, _) => "save",
        (_, Side::Home, _) => "restore",
    }
}

fn perform(action: &Action, layout: &Layout, backup: Option<&mut Backup>) -> Result<()> {
    let rel = &action.rel;
    // Removed directories are pruned up to the root of the side written.
    let root = match action.side {
        Side::Home => &layout.home,
        Side::Store => &layout.store,
    };
    match action.op {
        Op::Remove => {
            if let Some(meta) = fsx::lstat(&action.dst)?
                && let Some(b) = backup
            {
                b.stash(rel, &action.dst, &meta)?;
            }
            fsx::remove_entry(&action.dst, root)
        }
        Op::Create | Op::Overwrite => {
            let src = action
                .src
                .as_ref()
                .ok_or_else(|| anyhow!("no source for {rel}"))?;
            // Look again rather than trusting the scan: things change.
            let src_meta = fsx::lstat(src)?.ok_or_else(|| anyhow!("{} vanished", src.display()))?;
            let dst_meta = fsx::lstat(&action.dst)?;
            if let Some(d) = &dst_meta
                && let Some(b) = backup
            {
                b.stash(rel, &action.dst, d)
                    .with_context(|| format!("cannot back up {}", action.dst.display()))?;
            }
            fsx::copy_entry(src, &src_meta, &action.dst, dst_meta.as_ref())
        }
    }
}
