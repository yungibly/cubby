//! Turn a scan into a list of actions, and carry them out.
//!
//! `save` copies home → store and, under tracked directories, removes store
//! files that were deleted at home. `restore` copies store → home and never
//! removes anything. Both back up whatever they overwrite or remove.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};

use crate::backup::Backup;
use crate::fsx;
use crate::history::History;
use crate::paths::{Layout, Rel};
use crate::scan::{Newer, Scan, State};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// home → store
    Save,
    /// store → home
    Restore,
}

impl Direction {
    pub fn verb(self) -> &'static str {
        match self {
            Direction::Save => "save",
            Direction::Restore => "restore",
        }
    }

    pub fn past(self) -> &'static str {
        match self {
            Direction::Save => "saved",
            Direction::Restore => "restored",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// Create the destination.
    Create,
    /// Replace the destination.
    Overwrite,
    /// Remove from the store (save only).
    Remove,
}

#[derive(Clone, Debug)]
pub struct Action {
    pub rel: Rel,
    pub op: Op,
    /// Short explanation shown next to the path.
    pub note: String,
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
    pub direction: Direction,
    pub actions: Vec<Action>,
    pub skipped: Vec<Skipped>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    pub fn count(&self, op: Op) -> usize {
        self.actions.iter().filter(|a| a.op == op).count()
    }
}

pub fn plan(scan: &Scan, direction: Direction, force: bool) -> Plan {
    let mut actions = Vec::new();
    let mut skipped = Vec::new();
    for e in &scan.entries {
        let rel = e.rel.clone();
        let skip = |why: Skip| Skipped {
            rel: rel.clone(),
            why,
        };
        match (&e.state, direction) {
            (State::Same, _) => {}
            (State::Modified(newer), dir) => {
                let warn = match (newer, dir) {
                    (Newer::Store, Direction::Save) => ", store copy is newer",
                    (Newer::Home, Direction::Restore) => ", home copy is newer",
                    _ => "",
                };
                actions.push(Action {
                    rel,
                    op: Op::Overwrite,
                    note: format!("modified{warn}"),
                });
            }
            (State::New, Direction::Save) => actions.push(Action {
                rel,
                op: Op::Create,
                note: "new".into(),
            }),
            (State::New, Direction::Restore) => skipped.push(skip(Skip::NewAtHome)),
            (State::Missing { deleted: true }, Direction::Save) => actions.push(Action {
                rel,
                op: Op::Remove,
                note: "deleted at home".into(),
            }),
            (State::Missing { deleted: false }, Direction::Save) => {
                skipped.push(skip(Skip::MissingAtHome))
            }
            (State::Missing { .. }, Direction::Restore) => actions.push(Action {
                rel,
                op: Op::Create,
                note: "missing at home".into(),
            }),
            (State::Conflict { home, store }, dir) => {
                let text = format!(
                    "home has {}, store has {}",
                    home.describe(),
                    store.describe()
                );
                let replaceable =
                    !matches!(home, fsx::Kind::Dir) && !matches!(store, fsx::Kind::Dir);
                if force && replaceable {
                    actions.push(Action {
                        rel,
                        op: Op::Overwrite,
                        note: match dir {
                            Direction::Save => format!("{text}; replacing the store copy"),
                            Direction::Restore => format!("{text}; replacing the home copy"),
                        },
                    });
                } else {
                    skipped.push(skip(Skip::Conflict(text)));
                }
            }
            (State::Error(msg), _) => skipped.push(skip(Skip::Error(msg.clone()))),
        }
    }
    Plan {
        direction,
        actions,
        skipped,
    }
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
        let result = perform(action, plan.direction, layout, backup.as_mut());
        match result {
            Ok(()) => {
                done += 1;
                history.record(op_name(action, plan.direction), &action.rel)?;
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

fn op_name(action: &Action, direction: Direction) -> &'static str {
    match (action.op, direction) {
        (Op::Remove, _) => "remove",
        (_, Direction::Save) => "save",
        (_, Direction::Restore) => "restore",
    }
}

fn perform(
    action: &Action,
    direction: Direction,
    layout: &Layout,
    backup: Option<&mut Backup>,
) -> Result<()> {
    let rel = &action.rel;
    match action.op {
        Op::Remove => {
            let path = layout.stored(rel);
            if let Some(meta) = fsx::lstat(&path)?
                && let Some(b) = backup
            {
                b.stash(rel, &path, &meta)?;
            }
            fsx::remove_entry(&path, &layout.store)
        }
        Op::Create | Op::Overwrite => {
            let (src, dst) = match direction {
                Direction::Save => (layout.live(rel), layout.stored(rel)),
                Direction::Restore => (layout.stored(rel), layout.live(rel)),
            };
            // Look again rather than trusting the scan: things change.
            let src_meta =
                fsx::lstat(&src)?.ok_or_else(|| anyhow!("{} vanished", src.display()))?;
            let dst_meta = fsx::lstat(&dst)?;
            if let Some(d) = &dst_meta
                && let Some(b) = backup
            {
                b.stash(rel, &dst, d)
                    .with_context(|| format!("cannot back up {}", dst.display()))?;
            }
            fsx::copy_entry(&src, &src_meta, &dst, dst_meta.as_ref())
        }
    }
}
