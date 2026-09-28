use anyhow::Result;

use super::Ctx;
use crate::fsx;
use crate::paths::Rel;
use crate::plan;
use crate::scan::Scope;
use crate::ui;

pub fn run(ctx: &mut Ctx, paths: &[String]) -> Result<i32> {
    ctx.require_store()?;
    ctx.require_lock()?;
    let (rels, mut failures) = ctx.resolve_paths(paths);

    // Work out what each path means before touching anything.
    let mut dirs_to_drop: Vec<Rel> = Vec::new();
    let mut scope_rels: Vec<Rel> = Vec::new();
    for rel in rels {
        if ctx.manifest.is_dir(&rel) {
            dirs_to_drop.push(rel.clone());
            scope_rels.push(rel);
            continue;
        }
        if let Some(dir) = ctx.manifest.dir_for(&rel) {
            ctx.error(&format!(
                "{rel} is inside the tracked directory {dir}; to stop tracking it, run `cubby ignore {rel}`"
            ));
            failures += 1;
            continue;
        }
        if fsx::lstat(&ctx.cfg.layout.stored(&rel))?.is_none() {
            ctx.error(&format!("{rel} is not tracked"));
            failures += 1;
            continue;
        }
        scope_rels.push(rel);
    }
    if scope_rels.is_empty() {
        return Ok(1);
    }

    let scan = ctx.shared_scanner().scan(&Scope::of(scope_rels.clone()))?;
    let plan = plan::untrack_plan(&scan);

    println!("{} {}", ctx.style.bold("untrack ←"), ctx.store_label());
    for d in &dirs_to_drop {
        println!(
            "{}",
            ui::row(
                &ctx.style,
                &ctx.style.red("-"),
                d.as_str(),
                "tracked directory"
            )
        );
    }
    ctx.print_plan(&plan);
    ctx.note(&format!(
        "  {} to remove from the store; home is left untouched",
        ui::plural(plan.actions.len(), "file", "files")
    ));

    if ctx.dry_run {
        ctx.note("dry run, nothing changed");
        return Ok(0);
    }
    if !ctx.confirm(&format!(
        "untrack {}?",
        ui::plural(plan.actions.len().max(dirs_to_drop.len()), "path", "paths")
    ))? {
        ctx.note("aborted");
        return Ok(1);
    }

    // The manifest changes with the files, as part of the same run: the
    // directories untracked, and permission records for what leaves the
    // store (untracked files, and directories that will hold nothing).
    for d in &dirs_to_drop {
        ctx.manifest.remove_dir(d);
    }
    let removed: Vec<&Rel> = plan.actions.iter().map(|a| &a.rel).collect();
    let remaining: Vec<Rel> = ctx
        .shared_scanner()
        .store_entries()?
        .into_iter()
        .map(|(rel, _)| rel)
        .filter(|rel| !removed.contains(&rel))
        .collect();
    let stale: Vec<Rel> = ctx
        .manifest
        .modes
        .keys()
        .filter(|r| scope_rels.iter().any(|s| r.is_within(s) || s.is_within(r)))
        .filter(|r| !remaining.iter().any(|rel| rel.is_within(r)))
        .cloned()
        .collect();
    for r in stale {
        ctx.manifest.set_mode(&r, None);
    }
    let code = ctx.run_plan(&plan)?;
    // Directories left empty in the store go too.
    for rel in &scope_rels {
        fsx::prune_empty_dirs(Some(&ctx.cfg.layout.stored(rel)), &ctx.cfg.layout.store);
    }
    Ok(if failures > 0 { 1 } else { code })
}
