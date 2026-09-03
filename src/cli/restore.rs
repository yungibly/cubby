use anyhow::Result;

use super::Ctx;
use crate::plan::{self, Direction, Op};
use crate::scan::Scope;
use crate::ui;

pub fn run(ctx: &Ctx, paths: &[String], force: bool) -> Result<i32> {
    ctx.require_store()?;
    let (rels, mut failures) = ctx.resolve_paths(paths);
    if !paths.is_empty() && rels.is_empty() {
        return Ok(1);
    }
    let scope = if paths.is_empty() {
        Scope::all()
    } else {
        Scope::of(rels)
    };
    let scan = ctx.scanner().scan(&scope)?;

    // Point out named paths that have nothing in the store.
    for rel in &scope.rels {
        if !scan
            .entries
            .iter()
            .any(|e| e.rel.is_within(rel) && e.store.is_some())
        {
            ctx.error(&format!("nothing in the store at {rel}"));
            failures += 1;
        }
    }

    let plan = plan::plan(&scan, &ctx.cfg.layout, Direction::Restore, force);
    for n in &scan.notes {
        ctx.warn(&format!("{}: {}", n.path, n.why));
    }

    if plan.is_empty() {
        ctx.print_skipped(&plan);
        println!(
            "{} {}",
            ctx.style.green("✓"),
            ctx.style.dim("nothing to restore, home is up to date")
        );
        return Ok(if failures > 0 { 1 } else { 0 });
    }

    println!("{} {}", ctx.style.bold("restore ←"), ctx.store_label());
    ctx.print_plan(&plan);
    ctx.print_skipped(&plan);
    let created = plan.count(Op::Create);
    let overwritten = plan.count(Op::Overwrite);
    let mut summary = ui::plural(created, "file to create", "files to create");
    if overwritten > 0 {
        summary.push_str(&format!(
            ", {}",
            ui::plural(overwritten, "file to overwrite", "files to overwrite")
        ));
    }
    ctx.note(&format!("  {summary}"));

    if ctx.dry_run {
        ctx.note("dry run, nothing changed");
        return Ok(0);
    }
    if !ctx.confirm(&format!(
        "restore {}?",
        ui::plural(plan.actions.len(), "file", "files")
    ))? {
        ctx.note("aborted");
        return Ok(1);
    }
    let code = ctx.run_plan(&plan)?;
    Ok(if failures > 0 { 1 } else { code })
}
