use anyhow::Result;

use super::Ctx;
use crate::fsx;
use crate::plan::{self, Plan, Side};
use crate::ui;

/// Copy every change in the direction it was made: what changed at home is
/// saved, what changed in the store is restored.
pub fn run(ctx: &mut Ctx, paths: &[String], allow_secrets: bool) -> Result<i32> {
    ctx.require_store()?;
    ctx.require_lock()?;
    let Some((scope, mut failures)) = ctx.scope_for(paths) else {
        return Ok(1);
    };
    let scan = ctx.scanner().scan(&scope)?;
    ctx.learn(&scan, &scope);
    failures += ctx.report_unstored(&scope, &scan);
    let mut plan = plan::sync_plan(&scan, &ctx.cfg.layout, &ctx.manifest.modes);
    failures += plan.troubled();
    let secrets = ctx.mark_secrets(&mut plan);
    let large = ctx.mark_large(&mut plan);

    for n in &scan.notes {
        ctx.warn(&format!("{}: {}", n.path, n.why));
    }
    for dir in &scan.empty_dirs {
        ctx.warn(&format!(
            "{dir} exists at home but has no files; leaving its store copy alone"
        ));
    }

    if plan.is_empty() {
        ctx.print_skipped(&plan);
        ctx.print_nothing_to_do(&plan, "nothing to sync, home and the store agree");
        return Ok(if failures > 0 { 1 } else { 0 });
    }

    println!("{} {}", ctx.style.bold("sync ↔"), ctx.store_label());
    let (to_store, to_home) = split(&plan);
    if !to_store.is_empty() {
        println!("{}", ctx.style.dim("  home → store"));
        ctx.print_plan(&to_store);
    }
    if !to_home.is_empty() {
        println!("{}", ctx.style.dim("  store → home"));
        ctx.print_plan(&to_home);
    }
    ctx.print_skipped(&plan);
    ctx.warn_secrets(&secrets);
    ctx.warn_large(large);

    let saves = to_store.actions.len() + to_store.standalone_records().count();
    let restores = to_home.actions.len();
    let mut parts = Vec::new();
    if saves > 0 {
        parts.push(format!(
            "{} to save ({})",
            ui::plural(saves, "change", "changes"),
            fsx::human_size(to_store.bytes_to_copy())
        ));
    }
    if restores > 0 {
        parts.push(format!(
            "{} to restore",
            ui::plural(restores, "change", "changes")
        ));
    }
    ctx.note(&format!("  {}", parts.join(", ")));

    if ctx.dry_run {
        ctx.note("dry run, nothing changed");
        return Ok(if failures > 0 { 1 } else { 0 });
    }
    if !ctx.confirm(&format!(
        "sync {}?",
        ui::plural(saves + restores, "change", "changes")
    ))? {
        ctx.note("aborted");
        return Ok(1);
    }
    failures += ctx.withhold_secrets(&mut plan, &secrets, allow_secrets)?;
    if plan.is_empty() {
        return Ok(1);
    }
    // The run writes the manifest, and records these edits for undo.
    for r in &plan.records {
        ctx.manifest.set_mode(&r.rel, r.to);
    }
    let code = ctx.run_plan(&plan)?;
    ctx.warn_git(&plan);
    Ok(if failures > 0 { 1 } else { code })
}

/// The plan's actions by the side they write, for showing; permission
/// records go with the store.
fn split(plan: &Plan) -> (Plan, Plan) {
    let mut to_store = Plan::new(plan.kind);
    let mut to_home = Plan::new(plan.kind);
    for a in &plan.actions {
        match a.side {
            Side::Store => to_store.actions.push(a.clone()),
            Side::Home => to_home.actions.push(a.clone()),
        }
    }
    to_store.records = plan.records.clone();
    (to_store, to_home)
}
