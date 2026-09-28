use anyhow::Result;

use super::Ctx;
use crate::perms;
use crate::scan::{Change, State};
use crate::ui;

pub fn run(ctx: &mut Ctx, paths: &[String], quiet: bool) -> Result<i32> {
    if quiet {
        // grep-style codes so a prompt can tell "differs" from "broken".
        return match run_quiet(ctx, paths) {
            Ok(code) => Ok(code),
            Err(e) => {
                ctx.error(&format!("{e:#}"));
                Ok(2)
            }
        };
    }
    ctx.require_store()?;
    let Some((scope, failures)) = ctx.scope_for(paths) else {
        return Ok(1);
    };
    let scan = ctx.scanner().scan(&scope)?;
    ctx.learn(&scan, &scope);
    let style = &ctx.style;

    // Named paths with nothing tracked beneath them.
    let mut found_any = scope.is_all();
    for rel in &scope.rels {
        if scan.entries.iter().any(|e| e.rel.is_within(rel)) {
            found_any = true;
        } else {
            let why = ctx.ignore.reason(rel);
            let note = why.as_deref().unwrap_or("nothing tracked here");
            println!("{}", ui::row(style, &style.dim("?"), rel.as_str(), note));
        }
    }
    if !found_any {
        return Ok(if failures > 0 { 1 } else { 0 });
    }

    let mut modified = Vec::new();
    let mut new = Vec::new();
    let mut untracked = Vec::new();
    let mut missing = Vec::new();
    let mut unstored = Vec::new();
    let mut conflicts = Vec::new();
    let mut errors = Vec::new();
    let mut same = 0;
    // Which commands would pick up the differences.
    let (mut for_save, mut for_restore, mut both_sides) = (false, false, false);
    for e in &scan.entries {
        let path = e.rel.as_str();
        let row = |symbol: String, note: &str| ui::row(style, &symbol, path, note);
        match &e.state {
            State::Same => same += 1,
            State::Modified(Change::Both) => {
                both_sides = true;
                conflicts.push(row(style.red("!"), "changed at home and in the store"));
            }
            State::Modified(change) => {
                let note = match change {
                    Change::Home => "changed at home",
                    Change::Store => "changed in the store",
                    _ => "",
                };
                for_save |= *change != Change::Store;
                for_restore |= *change != Change::Home;
                modified.push(row(style.yellow("~"), note));
            }
            State::New { .. } if e.dir.is_none() => {
                untracked.push(row(style.dim("?"), "not tracked"))
            }
            State::New { was_stored: false } => {
                for_save = true;
                new.push(row(style.green("+"), ""));
            }
            State::New { was_stored: true } => unstored.push(row(style.red("-"), "")),
            State::Missing {
                was_here: true,
                store_changed: true,
                ..
            } => {
                both_sides = true;
                conflicts.push(row(style.red("!"), "deleted at home, changed in the store"));
            }
            State::Missing {
                was_here,
                under_present_dir,
                ..
            } => {
                for_restore = true;
                for_save |= *was_here && *under_present_dir;
                // In a tracked directory that is absent or empty at home,
                // the directory's own line explains why nothing happens.
                let note = match (was_here, under_present_dir, &e.dir) {
                    (false, _, _) => "new in the store",
                    (true, false, Some(_)) => "",
                    (true, _, _) => "deleted at home",
                };
                missing.push(row(style.red("-"), note));
            }
            State::Conflict { home, store } => conflicts.push(ui::row(
                style,
                &style.red("!"),
                path,
                &format!(
                    "home has {}, store has {}",
                    home.describe(),
                    store.describe()
                ),
            )),
            State::Error(msg) => errors.push(ui::row(style, &style.red("!"), path, msg)),
        }
    }

    let mut permissions = Vec::new();
    let (mut to_record, mut to_tighten) = (0, 0);
    for p in &scan.perms {
        let path = if p.is_dir {
            format!("{}/", p.rel.as_str())
        } else {
            p.rel.as_str().to_owned()
        };
        let recorded = p.recorded.map_or("not recorded yet".to_owned(), |r| {
            format!("{} recorded", perms::show(r))
        });
        permissions.push(ui::row(
            style,
            &style.yellow("~"),
            &path,
            &format!("{} at home, {recorded}", perms::show(p.home)),
        ));
        to_record += usize::from(p.needs_record());
        to_tighten += usize::from(p.needs_chmod());
    }

    let sections: [(&str, &Vec<String>); 8] = [
        ("modified", &modified),
        ("new at home, not saved yet", &new),
        ("not tracked", &untracked),
        ("in the store, missing at home", &missing),
        ("deleted from the store, still at home", &unstored),
        ("permissions", &permissions),
        ("conflicts", &conflicts),
        ("errors", &errors),
    ];
    let mut printed = false;
    for (title, rows) in sections {
        if rows.is_empty() {
            continue;
        }
        if printed {
            println!();
        }
        println!("{}", style.bold(title));
        for r in rows {
            println!("{r}");
        }
        printed = true;
    }
    if ctx.verbose && same > 0 {
        if printed {
            println!();
        }
        println!("{}", style.bold("up to date"));
        for e in scan.entries.iter().filter(|e| e.state.is_same()) {
            println!("{}", ui::row(style, &style.dim("="), e.rel.as_str(), ""));
        }
        printed = true;
    }
    let dir_rows = scan
        .absent_dirs
        .iter()
        .map(|d| (d, "tracked directory, does not exist at home"))
        .chain(
            scan.empty_dirs
                .iter()
                .map(|d| (d, "tracked directory, empty at home; store copy kept")),
        );
    for (dir, note) in dir_rows {
        if printed {
            println!();
            printed = false;
        }
        println!("{}", ui::row(style, &style.red("-"), dir.as_str(), note));
    }
    for n in &scan.notes {
        if printed {
            println!();
            printed = false;
        }
        println!("{}", ui::row(style, &style.yellow("!"), &n.path, &n.why));
    }

    let changes = modified.len()
        + new.len()
        + missing.len()
        + unstored.len()
        + permissions.len()
        + conflicts.len()
        + errors.len();
    if changes == 0 && scan.absent_dirs.is_empty() && scan.empty_dirs.is_empty() {
        let what = if scope.is_all() {
            format!(
                "{} up to date",
                ui::plural(same, "tracked file", "tracked files")
            )
        } else {
            "up to date".to_owned()
        };
        println!("{} {}", style.green("✓"), style.dim(&what));
    } else {
        println!();
        let mut parts = Vec::new();
        if !modified.is_empty() {
            parts.push(format!("{} modified", modified.len()));
        }
        if !new.is_empty() {
            parts.push(format!("{} new", new.len()));
        }
        if !missing.is_empty() {
            parts.push(format!("{} missing", missing.len()));
        }
        if !unstored.is_empty() {
            parts.push(format!("{} deleted from the store", unstored.len()));
        }
        if !permissions.is_empty() {
            parts.push(ui::plural(permissions.len(), "permission", "permissions"));
        }
        if !conflicts.is_empty() {
            parts.push(ui::plural(conflicts.len(), "conflict", "conflicts"));
        }
        if !errors.is_empty() {
            parts.push(ui::plural(errors.len(), "error", "errors"));
        }
        parts.push(format!("{same} up to date"));
        println!("{}", style.dim(&parts.join(" · ")));
        let mut hints = Vec::new();
        if for_save || to_record > 0 {
            hints.push("`cubby` saves home → store");
        }
        if for_restore || to_tighten > 0 {
            hints.push("`cubby restore` copies store → home");
        }
        if !hints.is_empty() {
            println!("{}", style.dim(&hints.join(", ")));
        }
        if hints.len() == 2 {
            println!(
                "{}",
                style.dim("`cubby sync` does both, each path in the direction it changed")
            );
        }
        if both_sides {
            println!(
                "{}",
                style.dim(
                    "for a path changed on both sides, `cubby diff PATH` shows them and `--force` picks one"
                )
            );
        }
        if !unstored.is_empty() {
            println!(
                "{}",
                style.dim(
                    "a file deleted from the store is not saved again unless you pass `cubby save --force PATH`; delete it at home to finish the deletion"
                )
            );
        }
    }
    Ok(if failures > 0 { 1 } else { 0 })
}

fn run_quiet(ctx: &mut Ctx, paths: &[String]) -> Result<i32> {
    ctx.require_store()?;
    let Some((scope, 0)) = ctx.scope_for(paths) else {
        return Ok(2);
    };
    let scan = ctx.scanner().scan(&scope)?;
    ctx.learn(&scan, &scope);
    let dirty = scan
        .entries
        .iter()
        .any(|e| !e.state.is_same() && !e.state.is_untracked(e.dir.as_ref()))
        || !scan.absent_dirs.is_empty()
        || !scan.empty_dirs.is_empty()
        || !scan.perms.is_empty();
    Ok(if dirty { 1 } else { 0 })
}
