use anyhow::Result;

use super::Ctx;
use crate::runs;
use crate::ui;

pub fn run(ctx: &Ctx, count: usize, all: bool, kind: Option<&str>) -> Result<i32> {
    let style = &ctx.style;
    let runs = runs::list(&ctx.cfg.state_dir);
    let undone = runs::undone(&runs);
    let shown: Vec<_> = runs
        .iter()
        .filter(|r| kind.is_none_or(|k| r.file.kind == k))
        .collect();
    let legacy = ctx.cfg.state_dir.join("history.log");
    let legacy_note = || {
        if legacy.exists() {
            ctx.note(&format!(
                "earlier history, from cubby 2, is in {}",
                ctx.cfg.layout.pretty(&legacy)
            ));
        }
    };
    if shown.is_empty() {
        ctx.note("no history yet");
        legacy_note();
        return Ok(0);
    }
    let start = if all {
        0
    } else {
        shown.len().saturating_sub(count)
    };
    let tz = jiff::tz::TimeZone::system();
    for r in &shown[start..] {
        let when = r
            .time
            .to_zoned(tz.clone())
            .strftime("%Y-%m-%d %H:%M:%S")
            .to_string();
        let padded = format!("{:<8}", r.file.kind);
        let kind = match r.file.kind.as_str() {
            "save" => style.green(&padded),
            "restore" | "sync" => style.cyan(&padded),
            "untrack" | "undo" => style.red(&padded),
            _ => padded,
        };
        let mut what = vec![ui::plural(r.file.actions.len(), "change", "changes")];
        let copies = r.file.actions.iter().filter(|a| a.backup).count();
        if copies > 0 {
            what.push(format!("{copies} backed up"));
        }
        if !r.file.manifest.is_empty() {
            what.push("manifest edited".to_owned());
        }
        if let Some(of) = &r.file.undoes {
            what.push(format!("reverses {of}"));
        }
        if undone.contains(&r.file.id) {
            what.push("undone".to_owned());
        }
        println!(
            "{}  {kind}  {}  {}",
            style.dim(&when),
            what.join(" · "),
            style.dim(&r.file.id)
        );
        if ctx.verbose {
            for a in &r.file.actions {
                let symbol = match a.op.as_str() {
                    "create" => style.green("+"),
                    "remove" => style.red("-"),
                    _ => style.yellow("~"),
                };
                let side = if a.side == "home" {
                    "at home"
                } else {
                    "in the store"
                };
                println!(
                    "    {symbol} {} {}",
                    a.path,
                    style.dim(&format!("{} {side}", a.op))
                );
            }
            for m in &r.file.manifest {
                println!(
                    "    {} {}",
                    style.dim("manifest:"),
                    style.dim(&format!("{} {}", m.edit, m.value))
                );
            }
        }
    }
    let n = shown.len() - start;
    let mut summary = format!("{} shown", ui::plural(n, "run", "runs"));
    if n < shown.len() {
        summary.push_str(&format!(" of {} (use --all or --count)", shown.len()));
    }
    if !ctx.verbose {
        summary.push_str("; -v lists the files");
    }
    ctx.note(&summary);
    legacy_note();
    Ok(0)
}
