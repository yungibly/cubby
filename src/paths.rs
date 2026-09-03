//! Paths relative to the home directory, and the rules for turning what a
//! user typed on the command line into one of them.
//!
//! Everything cubby tracks is addressed by a [`Rel`]: a normalized path
//! relative to the home directory (`.config/nvim/init.lua`). The same `Rel`
//! names the file in the home directory and in the store, which is what makes
//! the store a mirror of home.

use std::fmt;
use std::path::{Component, Path, PathBuf};

use anyhow::{Result, anyhow, bail};

/// A normalized path relative to the home directory.
///
/// Invariants: non-empty, uses `/` separators, no `.` or `..` components,
/// no leading or trailing slash.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Rel(String);

impl Rel {
    /// Build a `Rel` from text such as `.zshrc`, `~/.config/nvim`, or
    /// `/.config/nvim`. Rejects anything that escapes home.
    pub fn parse(text: &str) -> Result<Rel> {
        let trimmed = text.trim();
        let stripped = trimmed
            .strip_prefix("~/")
            .or_else(|| trimmed.strip_prefix('/'))
            .unwrap_or(trimmed);
        let mut parts: Vec<&str> = Vec::new();
        for part in stripped.split('/') {
            match part {
                "" | "." => continue,
                ".." => bail!("path {text:?} escapes the home directory"),
                p => parts.push(p),
            }
        }
        if parts.is_empty() || trimmed == "~" {
            bail!("the home directory itself cannot be tracked");
        }
        Ok(Rel(parts.join("/")))
    }

    /// Build a `Rel` from a path already known to be inside `base`.
    pub fn from_path_under(base: &Path, path: &Path) -> Result<Rel> {
        let rel = path
            .strip_prefix(base)
            .map_err(|_| anyhow!("{} is not inside {}", path.display(), base.display()))?;
        let mut parts = Vec::new();
        for comp in rel.components() {
            match comp {
                Component::Normal(p) => parts.push(
                    p.to_str()
                        .ok_or_else(|| anyhow!("{} is not valid UTF-8", path.display()))?
                        .to_owned(),
                ),
                Component::CurDir => {}
                _ => bail!("{} is not inside {}", path.display(), base.display()),
            }
        }
        if parts.is_empty() {
            bail!("{} is the base directory itself", path.display());
        }
        Ok(Rel(parts.join("/")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The path this `Rel` names beneath `base`.
    pub fn under(&self, base: &Path) -> PathBuf {
        base.join(&self.0)
    }

    /// True when `self` is `ancestor` or lies beneath it.
    pub fn is_within(&self, ancestor: &Rel) -> bool {
        self == ancestor
            || (self.0.len() > ancestor.0.len()
                && self.0.starts_with(&ancestor.0)
                && self.0.as_bytes()[ancestor.0.len()] == b'/')
    }

    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }
}

impl fmt::Display for Rel {
    /// Displays as `~/path`, which is how paths are shown to the user.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "~/{}", self.0)
    }
}

/// Where home and the store live on disk, and how to map between them.
#[derive(Clone, Debug)]
pub struct Layout {
    /// The home directory, with symlinks resolved.
    pub home: PathBuf,
    /// The store directory, with symlinks resolved when it exists.
    pub store: PathBuf,
}

impl Layout {
    pub fn live(&self, rel: &Rel) -> PathBuf {
        rel.under(&self.home)
    }

    pub fn stored(&self, rel: &Rel) -> PathBuf {
        rel.under(&self.store)
    }

    /// Show an absolute path the way a user would type it (`~/...` when it is
    /// inside home).
    pub fn pretty(&self, path: &Path) -> String {
        match path.strip_prefix(&self.home) {
            Ok(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => path.display().to_string(),
        }
    }

    /// Turn a user-supplied path into a [`Rel`].
    ///
    /// Accepts `~/x`, absolute paths, and paths relative to the current
    /// directory. Symlinks in the *parent* directories are resolved so that,
    /// for example, `/var/...` and `/private/var/...` on macOS agree; the
    /// final component is kept as-is so symlinks can be tracked as symlinks.
    ///
    /// Refuses paths outside home, home itself, anything inside the store,
    /// and symlinks that point into the store (a `stow`-style setup, where
    /// copying would clobber the file with a link to itself).
    pub fn resolve(&self, arg: &str) -> Result<Rel> {
        let expanded = expand_tilde(arg, &self.home);
        let absolute = if expanded.is_absolute() {
            expanded
        } else {
            std::env::current_dir()?.join(expanded)
        };
        let normalized = normalize(&absolute);
        let resolved = resolve_parents(&normalized)?;

        if resolved == self.home {
            bail!("the home directory itself cannot be tracked");
        }
        if resolved.starts_with(&self.store) {
            bail!(
                "{} is inside the store ({}); cubby tracks files in your home directory, not in the store",
                arg,
                self.pretty(&self.store)
            );
        }
        let rel = Rel::from_path_under(&self.home, &resolved).map_err(|_| {
            anyhow!(
                "{arg} is outside your home directory ({})",
                self.home.display()
            )
        })?;

        if let Ok(meta) = std::fs::symlink_metadata(&resolved)
            && meta.file_type().is_symlink()
            && let Ok(target) = std::fs::canonicalize(&resolved)
            && target.starts_with(&self.store)
        {
            bail!(
                "{rel} is a symlink into the store ({}); cubby copies files rather than linking them, so it will not track it",
                self.pretty(&target)
            );
        }
        Ok(rel)
    }
}

/// Expand a leading `~` or `~/` to the home directory.
pub fn expand_tilde(arg: &str, home: &Path) -> PathBuf {
    if arg == "~" {
        home.to_path_buf()
    } else if let Some(rest) = arg.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(arg)
    }
}

/// Remove `.` and `..` components lexically from an absolute path.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Canonicalize the longest existing prefix of `path` and re-attach the rest.
/// The final component is never followed, so a symlink stays a symlink.
fn resolve_parents(path: &Path) -> Result<PathBuf> {
    let Some(parent) = path.parent() else {
        return Ok(path.to_path_buf());
    };
    let Some(name) = path.file_name() else {
        return Ok(path.to_path_buf());
    };
    let mut existing = parent.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = vec![name.to_owned()];
    while !existing.exists() {
        match (existing.file_name(), existing.parent()) {
            (Some(n), Some(p)) => {
                tail.push(n.to_owned());
                existing = p.to_path_buf();
            }
            _ => break,
        }
    }
    let mut resolved = std::fs::canonicalize(&existing)
        .map_err(|e| anyhow!("cannot resolve {}: {e}", existing.display()))?;
    for part in tail.iter().rev() {
        resolved.push(part);
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_normalizes() {
        assert_eq!(
            Rel::parse("~/.config//nvim/").unwrap().as_str(),
            ".config/nvim"
        );
        assert_eq!(Rel::parse("/.zshrc").unwrap().as_str(), ".zshrc");
        assert_eq!(Rel::parse("./a/./b").unwrap().as_str(), "a/b");
        assert!(Rel::parse("~").is_err());
        assert!(Rel::parse("").is_err());
        assert!(Rel::parse("../x").is_err());
        assert!(Rel::parse("a/../../x").is_err());
    }

    #[test]
    fn within_is_component_aware() {
        let nvim = Rel::parse(".config/nvim").unwrap();
        assert!(
            Rel::parse(".config/nvim/init.lua")
                .unwrap()
                .is_within(&nvim)
        );
        assert!(nvim.is_within(&nvim));
        assert!(!Rel::parse(".config/nvim2/x").unwrap().is_within(&nvim));
        assert!(!Rel::parse(".config").unwrap().is_within(&nvim));
    }

    #[test]
    fn display_and_components() {
        let r = Rel::parse(".config/nvim/init.lua").unwrap();
        assert_eq!(r.to_string(), "~/.config/nvim/init.lua");
        assert_eq!(
            r.components().collect::<Vec<_>>(),
            vec![".config", "nvim", "init.lua"]
        );
    }

    #[test]
    fn normalize_dots() {
        assert_eq!(
            normalize(Path::new("/a/b/../c/./d")),
            PathBuf::from("/a/c/d")
        );
        assert_eq!(normalize(Path::new("/a/../../b")), PathBuf::from("/b"));
    }
}
