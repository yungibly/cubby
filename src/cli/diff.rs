use anyhow::Result;

use super::Ctx;
use crate::scan::{Scope, State};
use crate::ui;

pub fn run(ctx: &Ctx, paths: &[String], reverse: bool, no_pager: bool) -> Result<i32> {
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

    let mut out = String::new();
    let mut shown = 0;
    for e in &scan.entries {
        if e.state.is_same() {
            continue;
        }
        if let State::New = e.state
            && e.dir.is_none()
        {
            // Not tracked; nothing in the store to compare with.
            continue;
        }
        let text = crate::diff::render(e, &ctx.cfg.layout, reverse, &ctx.style)?;
        if text.is_empty() {
            continue;
        }
        if shown > 0 {
            out.push('\n');
        }
        out.push_str(&text);
        shown += 1;
    }
    for (rel, why) in &scan.notes {
        ctx.warn(&format!("{rel}: {why}"));
    }
    if shown == 0 {
        println!(
            "{} {}",
            ctx.style.green("✓"),
            ctx.style.dim("no differences")
        );
        return Ok(if failures > 0 { 1 } else { 0 });
    }
    ui::page(&out, !no_pager);
    Ok(if failures > 0 { 1 } else { 0 })
}
