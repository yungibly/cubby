use anyhow::Result;

use super::Ctx;
use crate::backup::{self, Backup};
use crate::config;
use crate::fsx;
use crate::paths::Rel;
use crate::scan::Scope;
use crate::ui;

pub fn run(ctx: &mut Ctx, paths: &[String]) -> Result<i32> {
    ctx.require_store()?;
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
                "{rel} is inside the tracked directory {dir}; add an ignore pattern to {} or untrack {dir}",
                crate::manifest::FILE_NAME
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

    let scan = ctx.scanner().scan(&Scope::of(scope_rels.clone()))?;
    let files: Vec<(Rel, std::path::PathBuf)> = scan
        .entries
        .iter()
        .filter_map(|e| e.store.as_ref().map(|m| (e.rel.clone(), m.path.clone())))
        .collect();

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
    let cap = if ctx.verbose { usize::MAX } else { 40 };
    for (i, (rel, _)) in files.iter().enumerate() {
        if i == cap {
            ctx.note(&format!(
                "  … and {} more (use --verbose to list all)",
                files.len() - cap
            ));
            break;
        }
        println!(
            "{}",
            ui::row(&ctx.style, &ctx.style.red("-"), rel.as_str(), "")
        );
    }
    ctx.note(&format!(
        "  {} to remove from the store; home is left untouched",
        ui::plural(files.len(), "file", "files")
    ));

    if ctx.dry_run {
        ctx.note("dry run, nothing changed");
        return Ok(0);
    }
    if !ctx.confirm(&format!(
        "untrack {}?",
        ui::plural(files.len().max(dirs_to_drop.len()), "path", "paths")
    ))? {
        ctx.note("aborted");
        return Ok(1);
    }

    let history = ctx.history();
    let mut backup = ctx
        .cfg
        .backups
        .then(|| Backup::new(&ctx.cfg.state_dir, "untrack"));
    let mut removed = 0;
    for (rel, path) in &files {
        let result = (|| -> Result<()> {
            if let Some(meta) = fsx::lstat(path)?
                && let Some(b) = backup.as_mut()
            {
                b.stash(rel, path, &meta)?;
            }
            fsx::remove_entry(path, &ctx.cfg.layout.store)
        })();
        match result {
            Ok(()) => {
                removed += 1;
                history.record("untrack", rel)?;
            }
            Err(e) => {
                failures += 1;
                println!(
                    "{}",
                    ui::row(
                        &ctx.style,
                        &ctx.style.red("✗"),
                        rel.as_str(),
                        &format!("{e:#}")
                    )
                );
            }
        }
    }
    // Empty directories left behind (from tracked directories with no files
    // left) are pruned too.
    for rel in &scope_rels {
        fsx::prune_empty_dirs(Some(&ctx.cfg.layout.stored(rel)), &ctx.cfg.layout.store);
    }
    let mut manifest_changed = false;
    for d in &dirs_to_drop {
        manifest_changed |= ctx.manifest.remove_dir(d);
    }
    if manifest_changed {
        ctx.manifest.save(&ctx.cfg.layout.store)?;
    }

    let mut summary = format!(
        "removed {} from the store",
        ui::plural(removed, "file", "files")
    );
    if let Some(b) = &backup
        && b.count() > 0
    {
        summary.push_str(&format!(
            " · backed up to {}",
            ctx.cfg.layout.pretty(b.dir())
        ));
    }
    ctx.note(&summary);
    if ctx.cfg.backups
        && let Err(e) = backup::prune(&ctx.cfg.state_dir, config::BACKUP_SETS_TO_KEEP)
    {
        ctx.warn(&format!("could not prune old backups: {e:#}"));
    }
    Ok(if failures > 0 { 1 } else { 0 })
}
