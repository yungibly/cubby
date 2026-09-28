use anyhow::Result;

use super::Ctx;
use crate::plan::{self, Direction, Op};
use crate::ui;

pub fn run(ctx: &mut Ctx, paths: &[String], force: bool) -> Result<i32> {
    ctx.require_store()?;
    ctx.require_lock()?;
    let Some((scope, mut failures)) = ctx.scope_for(paths) else {
        return Ok(1);
    };
    let scan = ctx.scanner().scan(&scope)?;
    ctx.learn(&scan, &scope);
    failures += ctx.report_unstored(&scope, &scan);

    let plan = plan::plan(
        &scan,
        &ctx.cfg.layout,
        &ctx.manifest.modes,
        Direction::Restore,
        force,
    );
    failures += plan.troubled();
    for n in &scan.notes {
        ctx.warn(&format!("{}: {}", n.path, n.why));
    }

    if plan.is_empty() {
        ctx.print_skipped(&plan);
        ctx.print_nothing_to_do(&plan, "nothing to restore, home is up to date");
        return Ok(if failures > 0 { 1 } else { 0 });
    }

    println!("{} {}", ctx.style.bold("restore ←"), ctx.store_label());
    ctx.print_plan(&plan);
    ctx.print_skipped(&plan);
    let created = plan.count(Op::Create);
    let overwritten = plan.count(Op::Overwrite);
    let chmods = plan.count(Op::Chmod);
    let mut parts = Vec::new();
    if created > 0 || (overwritten == 0 && chmods == 0) {
        parts.push(ui::plural(created, "file to create", "files to create"));
    }
    if overwritten > 0 {
        parts.push(ui::plural(
            overwritten,
            "file to overwrite",
            "files to overwrite",
        ));
    }
    if chmods > 0 {
        parts.push(format!(
            "permissions of {} to tighten",
            ui::plural(chmods, "path", "paths")
        ));
    }
    ctx.note(&format!("  {}", parts.join(", ")));

    if ctx.dry_run {
        ctx.note("dry run, nothing changed");
        return Ok(if failures > 0 { 1 } else { 0 });
    }
    if !ctx.confirm(&format!(
        "restore {}?",
        ui::plural(plan.actions.len(), "change", "changes")
    ))? {
        ctx.note("aborted");
        return Ok(1);
    }
    let code = ctx.run_plan(&plan)?;
    Ok(if failures > 0 { 1 } else { code })
}
