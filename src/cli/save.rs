use anyhow::Result;

use super::Ctx;
use crate::fsx::{self, Kind};
use crate::manifest::AddDir;
use crate::plan::{self, Direction, Op};
use crate::scan::Scope;
use crate::ui;

pub fn run(ctx: &mut Ctx, paths: &[String], force: bool) -> Result<i32> {
    ctx.require_store()?;
    let (rels, mut failures) = ctx.resolve_paths(paths);
    if !paths.is_empty() && rels.is_empty() {
        return Ok(1);
    }

    // Directories named on the command line become tracked as a whole.
    let mut manifest_changed = false;
    let mut scope_rels = Vec::new();
    for rel in rels {
        if let Some(pattern) = ctx.ignore.reason(&rel) {
            ctx.error(&format!(
                "{rel} is ignored (pattern {pattern:?} in {})",
                crate::manifest::FILE_NAME
            ));
            failures += 1;
            continue;
        }
        let live = fsx::lstat(&ctx.cfg.layout.live(&rel))?;
        let stored = fsx::lstat(&ctx.cfg.layout.stored(&rel))?;
        match live.map(|m| m.kind) {
            Some(Kind::Dir) => match ctx.manifest.add_dir(rel.clone()) {
                AddDir::Added { absorbed } => {
                    manifest_changed = true;
                    let mut msg = format!("tracking {rel} as a directory");
                    if !absorbed.is_empty() {
                        let names: Vec<String> = absorbed.iter().map(|d| d.to_string()).collect();
                        msg.push_str(&format!(" (it now covers {})", names.join(", ")));
                    }
                    ctx.note(&msg);
                }
                AddDir::Covered(_) => {}
            },
            Some(Kind::File | Kind::Symlink) => {}
            Some(Kind::Other) => {
                ctx.error(&format!("{rel} is a special file and cannot be tracked"));
                failures += 1;
                continue;
            }
            None if stored.is_some() => {}
            None => {
                ctx.error(&format!("{rel} does not exist"));
                failures += 1;
                continue;
            }
        }
        scope_rels.push(rel);
    }
    if !paths.is_empty() && scope_rels.is_empty() {
        return Ok(1);
    }
    let scope = if paths.is_empty() {
        Scope::all()
    } else {
        Scope::of(scope_rels)
    };

    let scan = ctx.scanner().scan(&scope)?;
    let plan = plan::plan(&scan, &ctx.cfg.layout, Direction::Save, force);

    for n in &scan.notes {
        ctx.warn(&format!("{}: {}", n.path, n.why));
    }
    for dir in &scan.empty_dirs {
        ctx.warn(&format!(
            "{dir} exists at home but has no files; leaving its store copy alone (run `cubby untrack {dir}` if that was deliberate)"
        ));
    }
    if scope.is_all() {
        for dir in &scan.absent_dirs {
            ctx.note(&format!(
                "  {dir} is tracked but does not exist at home; leaving its store copy alone"
            ));
        }
    }

    if plan.is_empty() {
        ctx.print_skipped(&plan);
        if manifest_changed && !ctx.dry_run {
            ctx.manifest.save(&ctx.cfg.layout.store)?;
        }
        println!(
            "{} {}",
            ctx.style.green("✓"),
            ctx.style.dim("nothing to save, the store is up to date")
        );
        return Ok(if failures > 0 { 1 } else { 0 });
    }

    println!("{} {}", ctx.style.bold("save →"), ctx.store_label());
    ctx.print_plan(&plan);
    ctx.print_skipped(&plan);

    let copies = plan.count(Op::Create) + plan.count(Op::Overwrite);
    let removals = plan.count(Op::Remove);
    let bytes = plan::bytes_to_copy(&plan, &scan);
    let mut summary = format!(
        "{} ({})",
        ui::plural(copies, "file to copy", "files to copy"),
        fsx::human_size(bytes)
    );
    if removals > 0 {
        summary.push_str(&format!(
            ", {} from the store",
            ui::plural(removals, "file to remove", "files to remove")
        ));
    }
    ctx.note(&format!("  {summary}"));

    if ctx.dry_run {
        ctx.note("dry run, nothing changed");
        return Ok(0);
    }
    if !ctx.confirm(&format!(
        "save {}?",
        ui::plural(plan.actions.len(), "change", "changes")
    ))? {
        ctx.note("aborted");
        return Ok(1);
    }
    if manifest_changed {
        ctx.manifest.save(&ctx.cfg.layout.store)?;
    }
    let code = ctx.run_plan(&plan)?;
    Ok(if failures > 0 { 1 } else { code })
}
