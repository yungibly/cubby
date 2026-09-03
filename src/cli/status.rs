use anyhow::Result;

use super::Ctx;
use crate::scan::{Newer, Scope, State};
use crate::ui;

pub fn run(ctx: &Ctx, paths: &[String]) -> Result<i32> {
    ctx.require_store()?;
    let (rels, failures) = ctx.resolve_paths(paths);
    if !paths.is_empty() && rels.is_empty() {
        return Ok(1);
    }
    let scope = if paths.is_empty() {
        Scope::all()
    } else {
        Scope::of(rels)
    };
    let scan = ctx.scanner().scan(&scope)?;
    let style = &ctx.style;

    // Named paths with nothing tracked beneath them.
    let mut found_any = scope.is_all();
    for rel in &scope.rels {
        if scan.entries.iter().any(|e| e.rel.is_within(rel)) {
            found_any = true;
        } else {
            println!(
                "{}",
                ui::row(style, &style.dim("?"), rel.as_str(), "nothing tracked here")
            );
        }
    }
    if !found_any {
        return Ok(if failures > 0 { 1 } else { 0 });
    }

    let mut modified = Vec::new();
    let mut new = Vec::new();
    let mut untracked = Vec::new();
    let mut missing = Vec::new();
    let mut conflicts = Vec::new();
    let mut errors = Vec::new();
    let mut same = 0;
    for e in &scan.entries {
        let path = e.rel.as_str();
        match &e.state {
            State::Same => same += 1,
            State::Modified(newer) => {
                let note = match newer {
                    Newer::Home => "home is newer",
                    Newer::Store => "store is newer",
                    Newer::Unknown => "",
                };
                modified.push(ui::row(style, &style.yellow("~"), path, note));
            }
            State::New if e.dir.is_some() => new.push(ui::row(style, &style.green("+"), path, "")),
            State::New => untracked.push(ui::row(style, &style.dim("?"), path, "not tracked")),
            State::Missing { deleted } => {
                let note = if *deleted { "deleted at home" } else { "" };
                missing.push(ui::row(style, &style.red("-"), path, note));
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

    let sections: [(&str, &Vec<String>); 6] = [
        ("modified", &modified),
        ("new at home, not saved yet", &new),
        ("not tracked", &untracked),
        ("in the store, missing at home", &missing),
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

    let changes = modified.len() + new.len() + missing.len() + conflicts.len() + errors.len();
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
        if !conflicts.is_empty() {
            parts.push(ui::plural(conflicts.len(), "conflict", "conflicts"));
        }
        if !errors.is_empty() {
            parts.push(ui::plural(errors.len(), "error", "errors"));
        }
        parts.push(format!("{same} up to date"));
        println!("{}", style.dim(&parts.join(" · ")));
        let mut hints = Vec::new();
        if !modified.is_empty() || !new.is_empty() {
            hints.push("`cubby` saves home → store");
        }
        if !modified.is_empty() || !missing.is_empty() {
            hints.push("`cubby restore` copies store → home");
        }
        if !hints.is_empty() {
            println!("{}", style.dim(&hints.join(", ")));
        }
    }
    Ok(if failures > 0 { 1 } else { 0 })
}
