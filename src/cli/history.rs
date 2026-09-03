use anyhow::Result;

use super::Ctx;
use crate::ui;

pub fn run(ctx: &Ctx, count: usize, all: bool, op: Option<&str>) -> Result<i32> {
    let records = ctx.history().read()?;
    let filtered: Vec<_> = records
        .iter()
        .filter(|r| op.is_none_or(|o| r.op == o))
        .collect();
    if filtered.is_empty() {
        ctx.note("no history yet");
        return Ok(0);
    }
    let start = if all {
        0
    } else {
        filtered.len().saturating_sub(count)
    };
    let tz = jiff::tz::TimeZone::system();
    for r in &filtered[start..] {
        let when = r
            .time
            .to_zoned(tz.clone())
            .strftime("%Y-%m-%d %H:%M:%S")
            .to_string();
        let op = match r.op.as_str() {
            "save" => ctx.style.green(&format!("{:<8}", "save")),
            "restore" => ctx.style.cyan(&format!("{:<8}", "restore")),
            "remove" | "untrack" => ctx.style.red(&format!("{:<8}", r.op)),
            other => format!("{other:<8}"),
        };
        println!("{}  {op}  {}", ctx.style.dim(&when), r.rel);
    }
    let shown = filtered.len() - start;
    let mut summary = format!("{} shown", ui::plural(shown, "entry", "entries"));
    if shown < filtered.len() {
        summary.push_str(&format!(" of {} (use --all or --count)", filtered.len()));
    }
    ctx.note(&summary);
    Ok(0)
}
