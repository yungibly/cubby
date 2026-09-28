use anyhow::Result;

use super::Ctx;
use crate::scan::State;
use crate::ui;

/// Show what `cubby save` would change (or, reversed, what `cubby restore`
/// would), and nothing else: files that command would leave alone are
/// counted in a note rather than shown as deletions.
pub fn run(ctx: &Ctx, paths: &[String], reverse: bool, no_pager: bool) -> Result<i32> {
    ctx.require_store()?;
    let Some((scope, mut failures)) = ctx.scope_for(paths) else {
        return Ok(1);
    };
    let scan = ctx.scanner().scan(&scope)?;
    failures += ctx.report_unstored(&scope, &scan);

    let mut out = String::new();
    let mut shown = 0;
    let mut hidden = 0;
    for e in &scan.entries {
        let applies = match (&e.state, reverse) {
            (State::Same, _) => continue,
            // Not tracked; nothing in the store to compare with.
            (State::New, _) if e.dir.is_none() => continue,
            // Restore never deletes anything at home.
            (State::New, true) => false,
            // Save leaves a file that is only in the store alone unless it
            // was deleted from a tracked directory.
            (State::Missing { deleted: false }, false) => false,
            _ => true,
        };
        if !applies {
            hidden += 1;
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
    for n in &scan.notes {
        ctx.warn(&format!("{}: {}", n.path, n.why));
    }
    let note = (hidden > 0).then(|| {
        if reverse {
            format!(
                "{} only at home not shown: restore never deletes",
                ui::plural(hidden, "file", "files")
            )
        } else {
            format!(
                "{} only in the store not shown: saving leaves {} alone (`cubby diff -R` shows what restore would create)",
                ui::plural(hidden, "file", "files"),
                if hidden == 1 { "it" } else { "them" }
            )
        }
    });
    if shown == 0 {
        let done = match (&note, reverse) {
            (None, _) => "no differences",
            (Some(_), false) => "nothing to save",
            (Some(_), true) => "nothing to restore",
        };
        println!("{} {}", ctx.style.green("✓"), ctx.style.dim(done));
        if let Some(note) = note {
            ctx.note(&note);
        }
        return Ok(if failures > 0 { 1 } else { 0 });
    }
    if let Some(note) = note {
        out.push('\n');
        out.push_str(&ctx.style.dim(&note));
        out.push('\n');
    }
    ui::page(&out, !no_pager);
    Ok(if failures > 0 { 1 } else { 0 })
}
