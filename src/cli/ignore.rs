//! `cubby ignore`: edit the ignore patterns in the manifest (every machine)
//! or the skip list in the config file (this machine).

use std::path::Path;

use anyhow::{Result, bail};

use super::Ctx;
use crate::config;
use crate::ignore::{self, BUILTIN_NAMES, BUILTIN_ROOT};
use crate::manifest::FILE_NAME;
use crate::plan;
use crate::ui;

pub fn run(ctx: &mut Ctx, patterns: &[String], here: bool, remove: bool) -> Result<i32> {
    if patterns.is_empty() {
        if remove {
            bail!("name the patterns to remove");
        }
        return list(ctx);
    }
    let patterns = patterns
        .iter()
        .map(|p| normalize(ctx, p))
        .collect::<Result<Vec<_>>>()?;
    for p in &patterns {
        ignore::validate(p)?;
    }
    if here {
        skip_here(ctx, &patterns, remove)
    } else {
        ctx.require_store()?;
        ctx.require_lock()?;
        everywhere(ctx, &patterns, remove)
    }
}

/// Patterns are written relative to home. A path the shell expanded
/// (`/Users/me/.config/x` for `~/.config/x`) is turned back into that form.
fn normalize(ctx: &Ctx, raw: &str) -> Result<String> {
    let path = Path::new(raw);
    let home = &ctx.cfg.layout.home;
    if path.is_absolute() && path.starts_with(home) {
        return Ok(ctx.cfg.layout.pretty(path));
    }
    if path.is_absolute()
        && let Some(first) = path.components().nth(1)
        && home.components().nth(1) == Some(first)
    {
        bail!("{raw} is outside your home directory");
    }
    Ok(raw.to_owned())
}

/// Add patterns to (or remove them from) the manifest, then offer to
/// remove the store files that are now ignored.
fn everywhere(ctx: &mut Ctx, patterns: &[String], remove: bool) -> Result<i32> {
    let before = ctx.shared_scanner().store_entries()?;
    let mut changed = 0;
    for p in patterns {
        let quoted = config::toml_string(p);
        if remove {
            if ctx.manifest.remove_ignore(p) {
                ctx.note(&format!("no longer ignoring {quoted} (in {FILE_NAME})"));
                changed += 1;
            } else {
                ctx.warn(&format!("{quoted} is not an ignore pattern in {FILE_NAME}"));
            }
        } else if ctx.manifest.add_ignore(p) {
            ctx.note(&format!(
                "ignoring {quoted} on every machine (in {FILE_NAME})"
            ));
            changed += 1;
        } else {
            ctx.note(&format!("{quoted} is already ignored"));
        }
    }
    if changed == 0 {
        return Ok(if remove { 1 } else { 0 });
    }
    if ctx.dry_run {
        ctx.note("dry run, nothing changed");
        return Ok(0);
    }
    ctx.manifest.save(&ctx.cfg.layout.store)?;
    if remove {
        return Ok(0);
    }

    // Files already in the store stay there, invisible to cubby, unless
    // they are removed now.
    let now = super::ignore_rules(&ctx.cfg, &ctx.manifest, &[])?;
    let matched: Vec<_> = before
        .into_iter()
        .filter(|(rel, _)| now.is_ignored(rel))
        .map(|(rel, meta)| (rel, meta.path))
        .collect();
    if matched.is_empty() {
        return Ok(0);
    }
    let plan = plan::removal_plan(matched);
    println!(
        "{} already in the store {} ignored now:",
        ui::plural(plan.actions.len(), "file", "files"),
        if plan.actions.len() == 1 { "is" } else { "are" }
    );
    ctx.print_plan(&plan);
    if !ctx.confirm(&format!(
        "remove {} from the store? (home is left untouched)",
        if plan.actions.len() == 1 {
            "it"
        } else {
            "them"
        }
    ))? {
        ctx.note(
            "left in the store; cubby no longer sees them, so delete them with git if you want them gone",
        );
        return Ok(0);
    }
    ctx.run_plan(&plan)
}

/// Add patterns to (or remove them from) this machine's skip list.
fn skip_here(ctx: &Ctx, patterns: &[String], remove: bool) -> Result<i32> {
    let config = ctx.cfg.layout.pretty(&ctx.cfg.config_path);
    if ctx.dry_run {
        for p in patterns {
            let verb = if remove { "stop skipping" } else { "skip" };
            ctx.note(&format!(
                "would {verb} {} on this machine",
                config::toml_string(p)
            ));
        }
        ctx.note("dry run, nothing changed");
        return Ok(0);
    }
    let store = ctx.store_label();
    let outcome = config::edit_list(&ctx.cfg.config_path, &store, "skip", |list| {
        let mut changed = Vec::new();
        for p in patterns {
            let present = list.iter().any(|v| v.as_str() == Some(p.as_str()));
            if remove && present {
                crate::tomlx::remove_where(list, |v| v.as_str() == Some(p.as_str()));
                changed.push(p.clone());
            } else if !remove && !present {
                let end = list.len();
                crate::tomlx::insert_str(list, end, p);
                changed.push(p.clone());
            }
        }
        changed
    })?;
    for p in patterns {
        let quoted = config::toml_string(p);
        match (remove, outcome.contains(p)) {
            (false, true) => ctx.note(&format!("skipping {quoted} on this machine (in {config})")),
            (false, false) => ctx.note(&format!("{quoted} is already skipped here")),
            (true, true) => ctx.note(&format!("no longer skipping {quoted} (in {config})")),
            (true, false) => ctx.warn(&format!("{quoted} is not in the skip list in {config}")),
        }
    }
    Ok(if remove && outcome.is_empty() { 1 } else { 0 })
}

fn list(ctx: &Ctx) -> Result<i32> {
    let style = &ctx.style;
    let section = |title: &str, items: &[String]| {
        println!("{}", style.bold(title));
        if items.is_empty() {
            println!("  {}", style.dim("(none)"));
        }
        for p in items {
            println!("  {p}");
        }
    };
    let clean = |list: &[String]| -> Vec<String> {
        list.iter()
            .filter(|p| !p.trim().is_empty() && !p.trim().starts_with('#'))
            .cloned()
            .collect()
    };
    section(
        &format!("ignored on every machine ({FILE_NAME})"),
        &clean(&ctx.manifest.ignore),
    );
    println!();
    section(
        &format!(
            "skipped on this machine ({})",
            ctx.cfg.layout.pretty(&ctx.cfg.config_path)
        ),
        &clean(&ctx.cfg.skip),
    );
    println!();
    println!("{}", style.bold("built in"));
    println!("  {}", BUILTIN_NAMES.join(", "));
    println!(
        "  {} {}",
        BUILTIN_ROOT.join(", "),
        style.dim("(at the root of the store)")
    );
    println!(
        "  {}",
        style.dim("cubby's own configuration and state (history, backups)")
    );
    Ok(0)
}
