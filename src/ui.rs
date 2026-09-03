//! Terminal output: colors, prompts, the pager, and the tree view.

use std::collections::BTreeMap;
use std::io::{self, IsTerminal, Write};
use std::process::{Command, Stdio};

use anyhow::{Result, bail};

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ColorChoice {
    Auto,
    Always,
    Never,
}

#[derive(Clone, Copy, Debug)]
pub struct Style {
    pub enabled: bool,
}

impl Style {
    pub fn detect(choice: ColorChoice) -> Style {
        let enabled = match choice {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            ColorChoice::Auto => {
                if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
                    false
                } else if std::env::var_os("CLICOLOR_FORCE")
                    .is_some_and(|v| !v.is_empty() && v != "0")
                {
                    true
                } else {
                    io::stdout().is_terminal()
                        && std::env::var_os("TERM").is_none_or(|t| t != "dumb")
                }
            }
        };
        Style { enabled }
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.enabled && !text.is_empty() {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_owned()
        }
    }

    pub fn bold(&self, s: &str) -> String {
        self.paint("1", s)
    }
    pub fn dim(&self, s: &str) -> String {
        self.paint("2", s)
    }
    pub fn red(&self, s: &str) -> String {
        self.paint("31", s)
    }
    pub fn green(&self, s: &str) -> String {
        self.paint("32", s)
    }
    pub fn yellow(&self, s: &str) -> String {
        self.paint("33", s)
    }
    pub fn cyan(&self, s: &str) -> String {
        self.paint("36", s)
    }
}

/// Ask a yes/no question. Fails rather than hangs when stdin is not a
/// terminal, so scripts must pass `--yes`.
pub fn confirm(question: &str, style: &Style) -> Result<bool> {
    if !io::stdin().is_terminal() {
        bail!("{question} — stdin is not a terminal; pass --yes to skip the prompt");
    }
    print!("{} {} {} ", style.yellow("?"), question, style.dim("[y/N]"));
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    let answer = answer.trim().to_ascii_lowercase();
    Ok(answer == "y" || answer == "yes")
}

/// Send text through the user's pager when stdout is a terminal, otherwise
/// print it. Uses `$CUBBY_PAGER`, then `$PAGER`, then `less`.
pub fn page(text: &str, allow_pager: bool) {
    let stdout = io::stdout();
    if !allow_pager || !stdout.is_terminal() {
        let _ = stdout.lock().write_all(text.as_bytes());
        return;
    }
    let pager = std::env::var("CUBBY_PAGER")
        .or_else(|_| std::env::var("PAGER"))
        .ok()
        .filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| "less".to_owned());
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(&pager).stdin(Stdio::piped());
    if std::env::var_os("LESS").is_none() {
        cmd.env("LESS", "FRX");
    }
    match cmd.spawn() {
        Ok(mut child) => {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
        }
        Err(_) => {
            let _ = stdout.lock().write_all(text.as_bytes());
        }
    }
}

/// A line in a listing: symbol, path, optional note in dim.
pub fn row(style: &Style, symbol: &str, path: &str, note: &str) -> String {
    if note.is_empty() {
        format!("  {symbol} {path}")
    } else {
        format!("  {symbol} {path:<44} {}", style.dim(note))
    }
}

pub fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

/// A file tree built from slash-separated paths.
#[derive(Default)]
pub struct Tree {
    children: BTreeMap<String, Tree>,
    /// A note shown after the name (for example, "-> target").
    note: Option<String>,
    is_leaf: bool,
}

impl Tree {
    pub fn insert(&mut self, path: &str, note: Option<String>, leaf: bool) {
        let mut node = self;
        for part in path.split('/') {
            node = node.children.entry(part.to_owned()).or_default();
        }
        node.is_leaf = leaf;
        if note.is_some() {
            node.note = note;
        }
    }

    pub fn render(&self, root_label: &str, style: &Style) -> String {
        let mut out = format!("{}\n", style.bold(root_label));
        self.render_children("", style, &mut out);
        out
    }

    fn render_children(&self, prefix: &str, style: &Style, out: &mut String) {
        let mut dirs: Vec<(&String, &Tree)> = Vec::new();
        let mut files: Vec<(&String, &Tree)> = Vec::new();
        for (name, child) in &self.children {
            if child.is_leaf {
                files.push((name, child));
            } else {
                dirs.push((name, child));
            }
        }
        let all: Vec<(&String, &Tree)> = dirs.into_iter().chain(files).collect();
        let last = all.len().saturating_sub(1);
        for (i, (name, child)) in all.iter().enumerate() {
            let connector = if i == last {
                "└── "
            } else {
                "├── "
            };
            let label = if child.is_leaf {
                name.to_string()
            } else {
                style.bold(&format!("{name}/"))
            };
            let note = child
                .note
                .as_deref()
                .map(|n| format!(" {}", style.dim(n)))
                .unwrap_or_default();
            out.push_str(&format!(
                "{}{label}{note}\n",
                style.dim(&format!("{prefix}{connector}"))
            ));
            let child_prefix = if i == last {
                format!("{prefix}    ")
            } else {
                format!("{prefix}│   ")
            };
            child.render_children(&child_prefix, style, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_renders_dirs_first() {
        let mut t = Tree::default();
        t.insert(".zshrc", None, true);
        t.insert(".config/nvim/init.lua", None, true);
        t.insert(".config/nvim", Some("tracked".into()), false);
        t.insert(".config/fish/config.fish", None, true);
        let style = Style { enabled: false };
        let expected = "\
store
├── .config/
│   ├── fish/
│   │   └── config.fish
│   └── nvim/ tracked
│       └── init.lua
└── .zshrc
";
        assert_eq!(t.render("store", &style), expected);
    }

    #[test]
    fn style_off_is_plain() {
        let s = Style { enabled: false };
        assert_eq!(s.red("x"), "x");
        let s = Style { enabled: true };
        assert_eq!(s.red("x"), "\x1b[31mx\x1b[0m");
        assert_eq!(s.red(""), "");
        assert_eq!(plural(1, "file", "files"), "1 file");
        assert_eq!(plural(2, "file", "files"), "2 files");
    }
}
