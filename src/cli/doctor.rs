//! `cubby doctor`: problems that do not show up as differences between
//! home and the store.

use anyhow::Result;

use super::{Ctx, Global, LARGE_FILE};
use crate::fsx::{self, Kind};
use crate::paths::Rel;
use crate::{git, perms, secrets, ui};

/// One check's findings: a headline and the paths it concerns.
struct Finding {
    title: String,
    items: Vec<(String, String)>,
    hint: Option<String>,
}

pub fn run(global: &Global) -> Result<i32> {
    let style = crate::ui::Style::detect(global.color);
    let ok = |what: &str, detail: &str| {
        println!("  {} {:<12} {}", style.green("✓"), what, style.dim(detail));
    };
    let ctx = match Ctx::load(global) {
        Ok(ctx) => ctx,
        Err(e) => {
            println!("  {} {:<12} {e:#}", style.red("✗"), "setup");
            return Ok(1);
        }
    };
    let layout = &ctx.cfg.layout;
    let pretty = |p: &std::path::Path| layout.pretty(p);

    if ctx.cfg.config_path.exists() {
        ok("config", &pretty(&ctx.cfg.config_path));
    } else {
        ok(
            "config",
            &format!(
                "none; using {} (`cubby init` writes one)",
                ctx.store_label()
            ),
        );
    }
    if let Err(e) = ctx.require_store() {
        println!("  {} {:<12} {e:#}", style.red("✗"), "store");
        return Ok(1);
    }
    let entries = ctx.shared_scanner().store_entries()?;
    ok(
        "store",
        &format!(
            "{} ({}, {})",
            ctx.store_label(),
            ui::plural(entries.len(), "file", "files"),
            ui::plural(
                ctx.manifest.dirs.len(),
                "tracked directory",
                "tracked directories"
            )
        ),
    );

    let mut findings = Vec::new();
    check_git(&ctx, &entries, &mut findings, &ok);
    check_own_files(&ctx, &mut findings);
    check_contents(&entries, &mut findings);
    check_perms(&ctx, &mut findings)?;
    check_links_and_dirs(&ctx, &entries, &mut findings)?;

    if findings.is_empty() {
        println!("{} {}", style.green("✓"), style.dim("no problems found"));
        return Ok(0);
    }
    for f in &findings {
        println!("  {} {}", style.yellow("!"), f.title);
        for (path, note) in f
            .items
            .iter()
            .take(if ctx.verbose { usize::MAX } else { 10 })
        {
            println!("      {path}  {}", style.dim(note));
        }
        if f.items.len() > 10 && !ctx.verbose {
            println!(
                "      {}",
                style.dim(&format!("… and {} more (-v lists all)", f.items.len() - 10))
            );
        }
        if let Some(hint) = &f.hint {
            println!("      {}", style.dim(hint));
        }
    }
    println!(
        "{}",
        style.dim(&ui::plural(
            findings.len(),
            "problem found",
            "problems found"
        ))
    );
    Ok(1)
}

fn check_git(
    ctx: &Ctx,
    entries: &[(Rel, fsx::Meta)],
    findings: &mut Vec<Finding>,
    ok: &dyn Fn(&str, &str),
) {
    let store = &ctx.cfg.layout.store;
    let Some(state) = git::state(store) else {
        findings.push(Finding {
            title: "the store is not versioned with git, so nothing carries it to another machine"
                .into(),
            items: Vec::new(),
            hint: Some("`cubby git init`, then commit and push it somewhere private".into()),
        });
        return;
    };
    let mut pending = Vec::new();
    if state.changes > 0 {
        pending.push(ui::plural(
            state.changes,
            "change to commit",
            "changes to commit",
        ));
    }
    if state.ahead > 0 {
        pending.push(ui::plural(
            state.ahead as usize,
            "commit to push",
            "commits to push",
        ));
    }
    if state.upstream.is_none() {
        pending.push("no upstream to push to".to_owned());
    }
    if pending.is_empty() {
        ok("git", "committed and pushed");
    } else {
        findings.push(Finding {
            title: format!("the store has {}", pending.join(", ")),
            items: Vec::new(),
            hint: Some("`cubby git status` shows them".into()),
        });
    }
    let rels: Vec<Rel> = entries.iter().map(|(r, _)| r.clone()).collect();
    let ignored = git::ignored(store, &rels);
    if !ignored.is_empty() {
        findings.push(Finding {
            title: format!(
                "git ignores {} in the store, so {} never committed",
                ui::plural(ignored.len(), "file", "files"),
                if ignored.len() == 1 {
                    "it is"
                } else {
                    "they are"
                }
            ),
            items: ignored
                .into_iter()
                .map(|i| (i.rel.as_str().to_owned(), i.rule))
                .collect(),
            hint: Some("change the rule, or keep that file out of a git-ignored name".into()),
        });
    }
}

/// cubby 2 could save its own config and state into the store.
fn check_own_files(ctx: &Ctx, findings: &mut Vec<Finding>) {
    let layout = &ctx.cfg.layout;
    let items: Vec<(String, String)> = ctx
        .cfg
        .own_paths()
        .into_iter()
        .filter_map(|(path, why)| {
            let rel = Rel::from_path_under(&layout.home, &path).ok()?;
            let stored = layout.stored(&rel);
            stored
                .exists()
                .then(|| (layout.pretty(&stored), why.to_owned()))
        })
        .collect();
    if !items.is_empty() {
        findings.push(Finding {
            title: "the store holds cubby's own files (an older cubby saved them)".into(),
            items,
            hint: Some("remove them from the store; cubby leaves them alone now".into()),
        });
    }
}

fn check_contents(entries: &[(Rel, fsx::Meta)], findings: &mut Vec<Finding>) {
    let secrets: Vec<(String, String)> = entries
        .iter()
        .filter_map(|(rel, meta)| {
            secrets::check_content(meta).map(|why| (rel.as_str().to_owned(), why))
        })
        .collect();
    if !secrets.is_empty() {
        findings.push(Finding {
            title: format!(
                "{} in the store {} like {}",
                ui::plural(secrets.len(), "file", "files"),
                if secrets.len() == 1 { "looks" } else { "look" },
                if secrets.len() == 1 {
                    "a secret"
                } else {
                    "secrets"
                }
            ),
            items: secrets,
            hint: Some("make sure wherever the store is pushed is private".into()),
        });
    }
    let large: Vec<(String, String)> = entries
        .iter()
        .filter(|(_, m)| m.kind == Kind::File && m.len > LARGE_FILE)
        .map(|(rel, m)| (rel.as_str().to_owned(), fsx::human_size(m.len)))
        .collect();
    if !large.is_empty() {
        findings.push(Finding {
            title: format!(
                "{} in the store {} over {}",
                ui::plural(large.len(), "file", "files"),
                if large.len() == 1 { "is" } else { "are" },
                fsx::human_size(LARGE_FILE)
            ),
            items: large,
            hint: Some("if they are caches, `cubby ignore PATTERN` drops them".into()),
        });
    }
}

fn check_perms(ctx: &Ctx, findings: &mut Vec<Finding>) -> Result<()> {
    let scan = ctx.scanner().scan(&crate::scan::Scope::all())?;
    let items: Vec<(String, String)> = scan
        .perms
        .iter()
        .map(|p| {
            let path = if p.is_dir {
                format!("{}/", p.rel.as_str())
            } else {
                p.rel.as_str().to_owned()
            };
            let note = match p.recorded {
                None => format!(
                    "{} at home, not recorded (`cubby` records it)",
                    perms::show(p.home)
                ),
                Some(r) => format!(
                    "{} at home, {} recorded (`cubby restore` tightens it)",
                    perms::show(p.home),
                    perms::show(r)
                ),
            };
            (path, note)
        })
        .collect();
    if !items.is_empty() {
        findings.push(Finding {
            title: "permissions at home differ from what the store records".into(),
            items,
            hint: None,
        });
    }
    // Store copies of private files that others can read.
    let loose: Vec<(String, String)> = ctx
        .manifest
        .modes
        .iter()
        .filter_map(|(rel, rec)| {
            let path = ctx.cfg.layout.stored(rel);
            let mode = std::fs::metadata(&path).ok()?.permissions();
            let mode = std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777;
            (mode & 0o077 & !rec != 0).then(|| {
                (
                    ctx.cfg.layout.pretty(&path),
                    format!(
                        "{} in the store, {} recorded: `chmod {} {}`",
                        perms::show(mode),
                        perms::show(*rec),
                        perms::show(perms::restrict(mode, *rec)),
                        ctx.cfg.layout.pretty(&path)
                    ),
                )
            })
        })
        .collect();
    if !loose.is_empty() {
        findings.push(Finding {
            title: "copies of private files in the store can be read by other users".into(),
            items: loose,
            hint: None,
        });
    }
    Ok(())
}

fn check_links_and_dirs(
    ctx: &Ctx,
    entries: &[(Rel, fsx::Meta)],
    findings: &mut Vec<Finding>,
) -> Result<()> {
    let layout = &ctx.cfg.layout;
    let mut links = Vec::new();
    for (rel, meta) in entries {
        if meta.kind == Kind::Symlink
            && let Some(home) = fsx::lstat(&layout.live(rel))?
            && home.points_to_dir
        {
            let target = meta.target.as_ref().map(|t| t.display().to_string());
            links.push((
                rel.as_str().to_owned(),
                format!("-> {}", target.unwrap_or_default()),
            ));
        }
    }
    if !links.is_empty() {
        findings.push(Finding {
            title: "these are saved as links to directories, not the files in them".into(),
            items: links,
            hint: Some("to keep the files, track the directory where it really is".into()),
        });
    }
    let empty: Vec<(String, String)> = ctx
        .manifest
        .dirs
        .iter()
        .filter(|d| !layout.live(d).exists() && !entries.iter().any(|(r, _)| r.is_within(d)))
        .map(|d| (d.to_string(), "neither at home nor in the store".to_owned()))
        .collect();
    if !empty.is_empty() {
        findings.push(Finding {
            title: "tracked directories with nothing in them anywhere".into(),
            items: empty,
            hint: Some("`cubby untrack DIR` stops tracking one".into()),
        });
    }
    Ok(())
}
