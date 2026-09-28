//! `cubby backups`: where the copies of overwritten and removed files are.

use anyhow::Result;

use super::Ctx;
use crate::fsx;
use crate::runs;
use crate::ui;

pub fn run(ctx: &Ctx, path: Option<&str>) -> Result<i32> {
    let style = &ctx.style;
    let state = &ctx.cfg.state_dir;
    let runs = runs::list(state);
    let legacy = runs::legacy_sets(state);
    let tz = jiff::tz::TimeZone::system();
    let when = |t: jiff::Timestamp| {
        t.to_zoned(tz.clone())
            .strftime("%Y-%m-%d %H:%M:%S")
            .to_string()
    };
    let pretty = |p: &std::path::Path| ctx.cfg.layout.pretty(p);

    let Some(path) = path else {
        let mut any = false;
        for r in runs.iter().rev() {
            let (n, size) = r.copies();
            if n == 0 {
                continue;
            }
            any = true;
            println!(
                "{}  {:<8}  {:>16}  {}",
                style.dim(&when(r.time)),
                r.file.kind,
                format!(
                    "{}, {}",
                    ui::plural(n, "file", "files"),
                    fsx::human_size(size)
                ),
                pretty(&r.dir.join("backup"))
            );
        }
        for set in legacy.iter().rev() {
            any = true;
            println!("{}  {}", style.dim(&set.name), pretty(&set.dir));
        }
        if !any {
            ctx.note("no backups yet");
            return Ok(0);
        }
        ctx.note(&format!(
            "kept for {} days, and the newest {} runs of each kind whatever their age; `cubby backups PATH` finds one file's copies",
            ctx.cfg.backup_days,
            runs::KEEP_PER_KIND
        ));
        return Ok(0);
    };

    let (rels, failures) = ctx.resolve_paths(&[path.to_owned()]);
    let Some(rel) = rels.first() else {
        return Ok(if failures > 0 { 1 } else { 0 });
    };
    let mut found = 0;
    for r in runs.iter().rev() {
        if let Some(copy) = r.backup_of(rel) {
            found += 1;
            let size = fsx::lstat(&copy)?.map_or(0, |m| m.len);
            println!(
                "{}  {:<8}  {:>9}  {}",
                style.dim(&when(r.time)),
                r.file.kind,
                fsx::human_size(size),
                pretty(&copy)
            );
        }
    }
    for set in legacy.iter().rev() {
        let copy = rel.under(&set.dir);
        if fsx::lstat(&copy)?.is_some() {
            found += 1;
            println!("{}  {}", style.dim(&set.name), pretty(&copy));
        }
    }
    if found == 0 {
        ctx.note(&format!("no backups of {rel}"));
    } else {
        ctx.note(&format!(
            "{} of {rel}, newest first",
            ui::plural(found, "copy", "copies")
        ));
    }
    Ok(0)
}
