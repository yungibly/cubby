//! The store is usually a git repository. cubby never commits or pushes,
//! but it looks: at saved files git would never commit, and at whether the
//! store has changes to commit or push. It also clones a store for a new
//! machine and passes commands through to git.
//!
//! Everything here is best-effort: without git, or outside a repository,
//! the answers are simply absent.

use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::paths::Rel;

fn git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir);
    cmd
}

/// Where the store is versioned: the repository's top level, and the
/// store's place inside it (empty when the store is the top level).
struct Repo {
    top: PathBuf,
    within: PathBuf,
}

/// The repository that versions the store, if any. A store inside some
/// other repository that ignores it (a home directory kept in git, say)
/// is not versioned by it.
fn repo(store: &Path) -> Option<Repo> {
    let out = git(store)
        .args(["rev-parse", "--show-toplevel"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let top = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n'));
    let top = std::fs::canonicalize(&top).unwrap_or(top);
    let store = std::fs::canonicalize(store).ok()?;
    let within = store.strip_prefix(&top).ok()?.to_path_buf();
    if !within.as_os_str().is_empty() {
        let ignored = git(&top)
            .args(["check-ignore", "-q", "--no-index"])
            .arg(&within)
            .stderr(Stdio::null())
            .status()
            .ok()?;
        if ignored.success() {
            return None;
        }
    }
    Some(Repo { top, within })
}

/// A saved path git would never commit, and the rule that says so.
pub struct Ignored {
    pub rel: Rel,
    /// `pattern` from `source:line`.
    pub rule: String,
}

/// Which of `rels` (paths in the store) git ignores and does not already
/// track: `git add` would skip them, so they never reach another machine.
pub fn ignored(store: &Path, rels: &[Rel]) -> Vec<Ignored> {
    if rels.is_empty() {
        return Vec::new();
    }
    let Some(repo) = repo(store) else {
        return Vec::new();
    };
    let child = git(&repo.top)
        .args(["check-ignore", "--verbose", "-z", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return Vec::new();
    };
    // Write the paths from another thread while this one reads the
    // answers: with enough of both, each side would fill its pipe and wait
    // for the other forever.
    let mut input = Vec::new();
    for rel in rels {
        input.extend_from_slice(repo.within.join(rel.as_str()).as_os_str().as_bytes());
        input.push(0);
    }
    let writer = child.stdin.take().map(|mut stdin| {
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        })
    });
    let out = child.wait_with_output();
    if let Some(w) = writer {
        let _ = w.join();
    }
    let Ok(out) = out else {
        return Vec::new();
    };
    // Records of four fields: source, line number, pattern, path.
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    let mut fields = out.stdout.split(|b| *b == 0);
    let mut found = Vec::new();
    while let (Some(source), Some(line), Some(pattern), Some(path)) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    {
        // --verbose also reports paths whose last match is a negation
        // (`!keep.conf`): those are not ignored.
        if pattern.starts_with(b"!") {
            continue;
        }
        let path = PathBuf::from(text(path));
        let Some(rel) = path
            .strip_prefix(&repo.within)
            .ok()
            .and_then(|p| Rel::parse(&p.to_string_lossy()).ok())
        else {
            continue;
        };
        found.push(Ignored {
            rel,
            rule: format!("{} in {}:{}", text(pattern), text(source), text(line)),
        });
    }
    found
}

/// What git says about the store.
pub struct State {
    /// Paths with changes not yet committed (including new ones).
    pub changes: usize,
    pub branch: Option<String>,
    pub upstream: Option<String>,
    /// Commits to push and to pull, as of the last fetch.
    pub ahead: u64,
    pub behind: u64,
}

/// The store's git status, or `None` when it is not a repository (or git
/// is not installed).
pub fn state(store: &Path) -> Option<State> {
    let repo = repo(store)?;
    let mut cmd = git(&repo.top);
    cmd.args([
        "status",
        "--porcelain=v2",
        "--branch",
        "--untracked-files=all",
        "-z",
    ]);
    // In a larger repository, only the store's part of it counts.
    if !repo.within.as_os_str().is_empty() {
        cmd.arg("--").arg(&repo.within);
    }
    let out = cmd.stderr(Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let mut state = State {
        changes: 0,
        branch: None,
        upstream: None,
        ahead: 0,
        behind: 0,
    };
    let records: Vec<&[u8]> = out.stdout.split(|b| *b == 0).collect();
    let mut i = 0;
    while i < records.len() {
        let line = String::from_utf8_lossy(records[i]);
        if let Some(head) = line.strip_prefix("# branch.head ") {
            state.branch = (head != "(detached)").then(|| head.to_owned());
        } else if let Some(up) = line.strip_prefix("# branch.upstream ") {
            state.upstream = Some(up.to_owned());
        } else if let Some(ab) = line.strip_prefix("# branch.ab ") {
            for part in ab.split(' ') {
                if let Some(n) = part.strip_prefix('+') {
                    state.ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = part.strip_prefix('-') {
                    state.behind = n.parse().unwrap_or(0);
                }
            }
        } else if line.starts_with("1 ") || line.starts_with("u ") || line.starts_with("? ") {
            state.changes += 1;
        } else if line.starts_with("2 ") {
            // A rename is followed by the path it came from.
            state.changes += 1;
            i += 1;
        }
        i += 1;
    }
    Some(state)
}

/// Whether `text` names a repository to clone rather than a directory:
/// a URL (`https://…`, `ssh://…`, `file://…`) or scp-style `user@host:path`.
pub fn is_url(text: &str) -> bool {
    if text.contains("://") {
        return true;
    }
    match (text.find('@'), text.find(':')) {
        (Some(at), Some(colon)) => at < colon && !text[..colon].contains('/'),
        _ => false,
    }
}

/// Clone `url` into `dest`, showing git's own progress and prompts.
pub fn clone(url: &str, dest: &Path) -> Result<()> {
    let status = Command::new("git")
        .arg("clone")
        .arg(url)
        .arg(dest)
        .status()
        .context("cannot run git; is it installed?")?;
    if !status.success() {
        bail!("git clone {url} failed");
    }
    Ok(())
}

/// Run git in the store with the terminal attached, returning its exit
/// code.
pub fn passthrough(store: &Path, args: &[std::ffi::OsString]) -> Result<i32> {
    let status = git(store)
        .args(args)
        .status()
        .context("cannot run git; is it installed?")?;
    Ok(status.code().unwrap_or(1))
}

/// Paths at the root of the store that git reads as settings for the store
/// repository itself.
pub fn is_repo_setting(rel: &Rel) -> bool {
    matches!(
        rel.as_str(),
        ".gitignore" | ".gitattributes" | ".gitmodules"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_and_directories_are_told_apart() {
        for url in [
            "https://github.com/me/dotfiles",
            "git@github.com:me/dotfiles.git",
            "ssh://git@host/dotfiles",
            "file:///tmp/dots",
            "me@host:dotfiles",
        ] {
            assert!(is_url(url), "{url}");
        }
        for dir in ["~/.dotfiles", "dots", "/tmp/a:b", "./me@x", "~/c:d"] {
            assert!(!is_url(dir), "{dir}");
        }
    }
}
