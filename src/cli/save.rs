use anyhow::Result;

use super::Ctx;
use crate::fsx::{self, Kind};
use crate::manifest::AddDir;
use crate::plan::{self, Direction, Op};
use crate::scan::Scope;
use crate::ui;

pub fn run(ctx: &mut Ctx, paths: &[String], force: bool) -> Result<i32> {
    ctx.require_store()?;
    ctx.require_lock()?;
    let (rels, mut failures) = ctx.resolve_paths(paths);
    if !paths.is_empty() && rels.is_empty() {
        return Ok(1);
    }

    // Directories named on the command line become tracked as a whole.
    let mut manifest_changed = false;
    let mut scope_rels = Vec::new();
    for rel in rels {
        if let Some(reason) = ctx.ignore.reason(&rel) {
            ctx.error(&format!("{rel} is ignored: {reason}"));
            failures += 1;
            continue;
        }
        let live = fsx::lstat(&ctx.cfg.layout.live(&rel))?;
        let stored = fsx::lstat(&ctx.cfg.layout.stored(&rel))?;
        if let Some(m) = &live
            && m.points_to_dir
        {
            warn_link_to_dir(ctx, &rel, m);
        }
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
                let name = rel.components().last().unwrap_or_default();
                let bare = paths.iter().any(|p| p == name);
                match super::similar_command(name).filter(|_| bare) {
                    Some(cmd) => ctx.error(&format!(
                        "{rel} does not exist (did you mean `cubby {cmd}`?)"
                    )),
                    None => ctx.error(&format!("{rel} does not exist")),
                }
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
    ctx.learn(&scan, &scope);
    let plan = plan::plan(
        &scan,
        &ctx.cfg.layout,
        &ctx.manifest.modes,
        Direction::Save,
        force,
    );
    failures += plan.troubled();

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
        ctx.print_nothing_to_do(&plan, "nothing to save, the store is up to date");
        return Ok(if failures > 0 { 1 } else { 0 });
    }

    println!("{} {}", ctx.style.bold("save →"), ctx.store_label());
    ctx.print_plan(&plan);
    ctx.print_skipped(&plan);

    let copies = plan.count(Op::Create) + plan.count(Op::Overwrite);
    let removals = plan.count(Op::Remove);
    let records = plan.records_set();
    let mut parts = Vec::new();
    if copies > 0 || records == 0 {
        parts.push(format!(
            "{} ({})",
            ui::plural(copies, "file to copy", "files to copy"),
            fsx::human_size(plan.bytes_to_copy())
        ));
    }
    if removals > 0 {
        parts.push(format!(
            "{} from the store",
            ui::plural(removals, "file to remove", "files to remove")
        ));
    }
    if records > 0 {
        parts.push(format!(
            "permissions of {} to record",
            ui::plural(records, "path", "paths")
        ));
    }
    ctx.note(&format!("  {}", parts.join(", ")));

    if ctx.dry_run {
        ctx.note("dry run, nothing changed");
        return Ok(if failures > 0 { 1 } else { 0 });
    }
    if !ctx.confirm(&format!(
        "save {}?",
        ui::plural(
            plan.actions.len() + plan.standalone_records().count(),
            "change",
            "changes"
        )
    ))? {
        ctx.note("aborted");
        return Ok(1);
    }
    // The run writes the manifest, and records these edits for undo.
    for r in &plan.records {
        ctx.manifest.set_mode(&r.rel, r.to);
    }
    let code = ctx.run_plan(&plan)?;
    Ok(if failures > 0 { 1 } else { code })
}

/// Links are copied as links, which is rarely what someone naming a
/// directory wants; say so and point at the real directory.
fn warn_link_to_dir(ctx: &Ctx, rel: &crate::paths::Rel, meta: &fsx::Meta) {
    let target = meta
        .target
        .as_ref()
        .map(|t| t.display().to_string())
        .unwrap_or_default();
    let real = std::fs::canonicalize(&meta.path).ok();
    let hint = match real
        .as_ref()
        .map(|r| (r, r.strip_prefix(&ctx.cfg.layout.home)))
    {
        Some((r, Ok(_))) if !r.starts_with(&ctx.cfg.layout.store) => format!(
            "to copy the directory's contents, track it where it really is: `cubby {}`",
            ctx.cfg.layout.pretty(r)
        ),
        _ => "cubby only copies directories inside your home directory".to_owned(),
    };
    ctx.warn(&format!(
        "{rel} is a symlink to a directory ({target}); cubby saves the link itself, not the files in it. {hint}"
    ));
}
