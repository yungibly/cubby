//! `cubby undo`: reverse a run, as far as what it left is still there.

use anyhow::{Result, bail};

use super::Ctx;
use crate::fsx::{self, Perms};
use crate::index::{self, Base};
use crate::paths::{Rel, Side};
use crate::plan::{Action, Op, Plan, RunKind};
use crate::runs::{self, Info, Recorded};
use crate::ui;

pub fn run(ctx: &mut Ctx, id: Option<&str>) -> Result<i32> {
    ctx.require_store()?;
    ctx.require_lock()?;
    let runs = runs::list(&ctx.cfg.state_dir);
    let undone = runs::undone(&runs);
    let target = match id {
        Some(id) => {
            let matches: Vec<&Info> = runs.iter().filter(|r| r.file.id.starts_with(id)).collect();
            match matches[..] {
                [one] => one,
                [] => bail!("no run {id}; `cubby history` lists them"),
                _ => bail!("{id} matches more than one run; give more of its id"),
            }
        }
        None => match runs
            .iter()
            .rev()
            .find(|r| r.file.kind != "undo" && !undone.contains(&r.file.id))
        {
            Some(r) => r,
            None => bail!("nothing to undo"),
        },
    };
    let id = target.file.id.clone();
    if undone.contains(&id) {
        bail!("{id} has already been undone; undo that undo to put its changes back");
    }
    if target.file.store != ctx.cfg.layout.store.display().to_string() {
        bail!(
            "{id} changed the store at {}; undo it with --store pointing there",
            target.file.store
        );
    }

    let mut plan = Plan::new(RunKind::Undo);
    plan.undoes = Some(id.clone());
    let mut bases = Vec::new();
    let mut left = Vec::new();
    for a in target.file.actions.iter().rev() {
        let Ok(rel) = Rel::parse(&a.path) else {
            continue;
        };
        match step(ctx, target, a, &rel)? {
            Ok(action) => {
                if let Some(base) = a.base.as_deref().and_then(Base::from_text) {
                    bases.push((rel, base));
                }
                plan.actions.push(action);
            }
            Err(why) => left.push((rel, why)),
        }
    }
    let mut manifest = 0;
    for e in target.file.manifest.iter().rev() {
        if let Some(edit) = e.to_edit()
            && ctx.manifest.reverse(&edit)
        {
            manifest += 1;
        }
    }

    let when = target
        .time
        .to_zoned(jiff::tz::TimeZone::system())
        .strftime("%Y-%m-%d %H:%M:%S");
    println!(
        "{} {} {}",
        ctx.style.bold("undo"),
        id,
        ctx.style.dim(&format!("({}, {when})", target.file.kind))
    );
    ctx.print_plan(&plan);
    for (rel, why) in &left {
        println!(
            "{}",
            ui::row(&ctx.style, &ctx.style.dim("·"), rel.as_str(), why)
        );
    }
    if manifest > 0 {
        ctx.note(&format!(
            "  {} in the manifest to reverse",
            ui::plural(manifest, "edit", "edits")
        ));
    }
    let changes = plan.actions.len() + manifest;
    if changes == 0 {
        println!(
            "{} {}",
            ctx.style.green("✓"),
            ctx.style
                .dim("nothing left to undo: everything that run changed has changed again since")
        );
        return Ok(if left.is_empty() { 0 } else { 1 });
    }
    if !left.is_empty() {
        ctx.note(&format!(
            "  {} changed since that run, so left alone",
            ui::plural(left.len(), "path", "paths")
        ));
    }
    if ctx.dry_run {
        ctx.note("dry run, nothing changed");
        return Ok(0);
    }
    if !ctx.confirm(&format!(
        "undo {}?",
        ui::plural(changes, "change", "changes")
    ))? {
        ctx.note("aborted");
        return Ok(1);
    }
    let code = ctx.run_plan(&plan)?;
    // Each path's last sync goes back to what it was before that run.
    for (rel, base) in bases {
        ctx.index.set_base(&rel, base);
    }
    if let Err(e) = ctx.index.save() {
        ctx.warn(&format!("could not update the index: {e:#}"));
    }
    Ok(if left.is_empty() { code } else { 1 })
}

/// The action that reverses one recorded action, or why it cannot be
/// reversed: the path changed since, or no copy of it was kept.
fn step(
    ctx: &Ctx,
    target: &Info,
    a: &Recorded,
    rel: &Rel,
) -> Result<std::result::Result<Action, String>> {
    let side = if a.side == "home" {
        Side::Home
    } else {
        Side::Store
    };
    let path = match side {
        Side::Home => ctx.cfg.layout.live(rel),
        Side::Store => ctx.cfg.layout.stored(rel),
    };
    let now = fsx::lstat(&path)?;
    let now_fp = now
        .as_ref()
        .filter(|m| m.kind != fsx::Kind::Dir)
        .and_then(|m| index::fingerprint(m).ok())
        .map(|f| f.hex());
    let unchanged = now.is_some() && now_fp == a.fp;
    let backup = if a.backup {
        target.backup_of(rel)
    } else {
        None
    };
    let action = |op: Op, src: Option<std::path::PathBuf>, note: &str, mode: Option<u32>| Action {
        rel: rel.clone(),
        op,
        side,
        note: note.to_owned(),
        src,
        dst: path.clone(),
        len: 0,
        perms: Perms::default(),
        mode,
    };
    let where_ = match side {
        Side::Home => "at home",
        Side::Store => "in the store",
    };
    Ok(match a.op.as_str() {
        "create" if now.is_none() => Err("already gone".into()),
        "create" if unchanged => Ok(action(
            Op::Remove,
            None,
            &format!("created {where_} by that run"),
            None,
        )),
        "overwrite" | "create" if !unchanged => Err("changed since".into()),
        "overwrite" => match backup {
            Some(b) => Ok(action(
                Op::Overwrite,
                Some(b),
                &format!("put back {where_}"),
                None,
            )),
            None => Err("no copy was kept (backups were off)".into()),
        },
        "remove" if now.is_some() => Err("there again since".into()),
        "remove" => match backup {
            Some(b) => Ok(action(
                Op::Create,
                Some(b),
                &format!("put back {where_}"),
                None,
            )),
            None => Err("no copy was kept (backups were off)".into()),
        },
        "chmod" => {
            let parse =
                |m: &Option<String>| m.as_deref().and_then(|t| u32::from_str_radix(t, 8).ok());
            let current = std::fs::metadata(&path)
                .ok()
                .map(|m| std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o777);
            match (parse(&a.mode_before), parse(&a.mode_after)) {
                (Some(before), Some(after)) if current == Some(after & 0o777) => Ok(action(
                    Op::Chmod,
                    None,
                    &format!(
                        "permissions {} → {}",
                        crate::perms::show(after),
                        crate::perms::show(before)
                    ),
                    Some(before),
                )),
                _ => Err("permissions changed since".into()),
            }
        }
        other => Err(format!("unknown action {other:?}")),
    })
}
