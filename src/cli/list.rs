use anyhow::Result;

use super::Ctx;
use crate::fsx::Kind;
use crate::paths::Rel;
use crate::perms;
use crate::ui::{self, Tree};

pub fn run(ctx: &Ctx, plain: bool) -> Result<i32> {
    ctx.require_store()?;
    let entries = ctx.scanner().store_entries()?;

    if plain {
        for (rel, _) in &entries {
            println!("{}", rel.as_str());
        }
        return Ok(0);
    }

    let mode = |rel: &Rel| {
        ctx.manifest
            .modes
            .get(rel)
            .map(|m| format!("mode {}", perms::show(*m)))
    };
    let mut tree = Tree::default();
    // Recorded directory modes first, so tracked directories can add to them.
    for rel in ctx.manifest.modes.keys() {
        if entries.iter().any(|(e, _)| e.is_within(rel) && e != rel) {
            tree.insert(rel.as_str(), mode(rel), false);
        }
    }
    for dir in &ctx.manifest.dirs {
        let note = match mode(dir) {
            Some(m) => format!("(tracked directory, {m})"),
            None => "(tracked directory)".to_owned(),
        };
        tree.insert(dir.as_str(), Some(note), false);
    }
    let mut symlinks = 0;
    for (rel, meta) in &entries {
        let mut notes = Vec::new();
        if meta.kind == Kind::Symlink {
            symlinks += 1;
            notes.push(format!(
                "-> {}",
                meta.target
                    .as_ref()
                    .map(|t| t.display().to_string())
                    .unwrap_or_default()
            ));
        }
        notes.extend(mode(rel));
        let note = (!notes.is_empty()).then(|| notes.join(", "));
        tree.insert(rel.as_str(), note, true);
    }

    if entries.is_empty() && ctx.manifest.dirs.is_empty() {
        println!(
            "{}",
            ctx.style.dim(&format!(
                "{} is empty; `cubby PATH` starts tracking something",
                ctx.store_label()
            ))
        );
        return Ok(0);
    }
    print!("{}", tree.render(&ctx.store_label(), &ctx.style));
    let mut summary = ui::plural(entries.len(), "file", "files");
    if symlinks > 0 {
        summary.push_str(&format!(
            " ({} {})",
            symlinks,
            if symlinks == 1 { "symlink" } else { "symlinks" }
        ));
    }
    if !ctx.manifest.dirs.is_empty() {
        summary.push_str(&format!(
            " · {}",
            ui::plural(
                ctx.manifest.dirs.len(),
                "tracked directory",
                "tracked directories"
            )
        ));
    }
    println!("{}", ctx.style.dim(&summary));
    Ok(0)
}
