//! Unified diffs between the home and store copies of a path.

use std::fmt::Write as _;

use anyhow::{Context, Result};
use similar::{Algorithm, ChangeTag, TextDiff};

use crate::fsx::{self, Kind};
use crate::paths::Layout;
use crate::scan::{Entry, State};
use crate::ui::Style;

/// Render the diff for one entry. By default the store copy is "old" and the
/// home copy is "new", so the output reads as what `cubby save` would
/// change; `reverse` flips it to what `cubby restore` would change.
pub fn render(entry: &Entry, layout: &Layout, reverse: bool, style: &Style) -> Result<String> {
    let rel = &entry.rel;
    let home = entry
        .home
        .as_ref()
        .map(|m| m.path.clone())
        .unwrap_or_else(|| layout.live(rel));
    let store = entry
        .store
        .as_ref()
        .map(|m| m.path.clone())
        .unwrap_or_else(|| layout.stored(rel));
    let (old_label, old_path, old_meta, new_label, new_path, new_meta) = if reverse {
        (
            "home",
            &home,
            entry.home.as_ref(),
            "store",
            &store,
            entry.store.as_ref(),
        )
    } else {
        (
            "store",
            &store,
            entry.store.as_ref(),
            "home",
            &home,
            entry.home.as_ref(),
        )
    };

    let mut out = String::new();
    let heading = |out: &mut String, note: &str| {
        let name = style.bold(&rel.to_string());
        let _ = if note.is_empty() {
            writeln!(out, "{name}")
        } else {
            writeln!(out, "{name} {}", style.dim(note))
        };
    };

    match &entry.state {
        State::Same => return Ok(String::new()),
        State::Error(msg) => {
            heading(&mut out, &format!("(error: {msg})"));
            return Ok(out);
        }
        State::Conflict { home, store } => {
            heading(
                &mut out,
                &format!(
                    "(home has {}, store has {})",
                    home.describe(),
                    store.describe()
                ),
            );
            return Ok(out);
        }
        State::New | State::Missing { .. } | State::Modified(_) => {}
    }

    let kind = old_meta.or(new_meta).map(|m| m.kind).unwrap_or(Kind::File);
    if kind == Kind::Symlink {
        let show = |m: Option<&fsx::Meta>| {
            m.and_then(|m| m.target.as_ref())
                .map(|t| t.display().to_string())
                .unwrap_or_else(|| "(absent)".into())
        };
        heading(&mut out, "(symlink)");
        let _ = writeln!(out, "{}", style.red(&format!("- {}", show(old_meta))));
        let _ = writeln!(out, "{}", style.green(&format!("+ {}", show(new_meta))));
        return Ok(out);
    }

    let old_binary = old_meta.is_some() && fsx::looks_binary(old_path);
    let new_binary = new_meta.is_some() && fsx::looks_binary(new_path);
    if old_binary || new_binary {
        heading(&mut out, "(binary files differ)");
        return Ok(out);
    }

    let old_text = read_or_empty(old_meta.is_some(), old_path)?;
    let new_text = read_or_empty(new_meta.is_some(), new_path)?;

    heading(&mut out, "");
    let _ = writeln!(out, "{}", style.bold(&format!("--- {old_label}")));
    let _ = writeln!(out, "{}", style.bold(&format!("+++ {new_label}")));

    let diff = TextDiff::configure()
        .algorithm(Algorithm::Patience)
        .diff_lines(old_text.as_str(), new_text.as_str());
    let mut unified = diff.unified_diff();
    unified.context_radius(3);
    for hunk in unified.iter_hunks() {
        let _ = writeln!(out, "{}", style.cyan(&hunk.header().to_string()));
        for change in hunk.iter_changes() {
            let (sign, paint): (&str, fn(&Style, &str) -> String) = match change.tag() {
                ChangeTag::Equal => (" ", |_, s| s.to_owned()),
                ChangeTag::Delete => ("-", Style::red),
                ChangeTag::Insert => ("+", Style::green),
            };
            let value = change.value();
            let line = value.strip_suffix('\n').unwrap_or(value);
            let _ = writeln!(out, "{}", paint(style, &format!("{sign}{line}")));
            if change.missing_newline() {
                let _ = writeln!(out, "{}", style.dim("\\ No newline at end of file"));
            }
        }
    }
    Ok(out)
}

fn read_or_empty(exists: bool, path: &std::path::Path) -> Result<String> {
    if !exists {
        return Ok(String::new());
    }
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
