use anyhow::Result;

use super::Ctx;
use crate::fsx::Kind;
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

    let mut tree = Tree::default();
    for dir in &ctx.manifest.dirs {
        tree.insert(dir.as_str(), Some("(tracked directory)".into()), false);
    }
    let mut symlinks = 0;
    for (rel, meta) in &entries {
        let note = (meta.kind == Kind::Symlink).then(|| {
            symlinks += 1;
            format!(
                "-> {}",
                meta.target
                    .as_ref()
                    .map(|t| t.display().to_string())
                    .unwrap_or_default()
            )
        });
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
