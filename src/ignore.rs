//! Ignore patterns.
//!
//! Two kinds of pattern, told apart by whether they contain a slash:
//!
//! * `name` patterns (`.DS_Store`, `*.swp`, `node_modules`) match the name of
//!   a file or directory at any depth.
//! * `path` patterns (`.config/nvim/lazy-lock.json`, `.config/**/cache`) match
//!   the whole path relative to home. A leading `~/` or `/` is ignored.
//!
//! A matched directory excludes everything beneath it. A few patterns are
//! built in because they are never dotfiles: `.git` directories, `.DS_Store`,
//! cubby's own temporary files, and at the root of the store the manifest,
//! `README*`, and `LICENSE*`.

use anyhow::{Context, Result};
use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};

use crate::paths::Rel;

/// Names ignored at any depth.
pub const BUILTIN_NAMES: &[&str] = &[".git", ".DS_Store", ".cubby-tmp-*"];
/// Names ignored only at the root of the store.
pub const BUILTIN_ROOT: &[&str] = &[crate::manifest::FILE_NAME, "README*", "LICENSE*"];

pub struct Ignore {
    names: GlobSet,
    paths: GlobSet,
    root: GlobSet,
    name_patterns: Vec<String>,
    path_patterns: Vec<String>,
}

impl Ignore {
    pub fn new(patterns: &[String]) -> Result<Ignore> {
        let mut names = GlobSetBuilder::new();
        let mut paths = GlobSetBuilder::new();
        let mut name_patterns = Vec::new();
        let mut path_patterns = Vec::new();

        for p in BUILTIN_NAMES {
            names.add(name_glob(p)?);
            name_patterns.push((*p).to_owned());
        }
        for raw in patterns {
            let p = raw.trim();
            if p.is_empty() || p.starts_with('#') {
                continue;
            }
            let stripped = p
                .strip_prefix("~/")
                .or_else(|| p.strip_prefix('/'))
                .unwrap_or(p);
            let stripped = stripped.strip_suffix('/').unwrap_or(stripped);
            if stripped.contains('/') {
                paths.add(path_glob(stripped)?);
                path_patterns.push(raw.clone());
            } else {
                names.add(name_glob(stripped)?);
                name_patterns.push(raw.clone());
            }
        }
        let mut root = GlobSetBuilder::new();
        for p in BUILTIN_ROOT {
            root.add(name_glob(p)?);
        }
        Ok(Ignore {
            names: names.build()?,
            paths: paths.build()?,
            root: root.build()?,
            name_patterns,
            path_patterns,
        })
    }

    /// Whether `rel` (a file or directory) is ignored. Every ancestor is
    /// checked too, so a file under an ignored directory is ignored.
    pub fn is_ignored(&self, rel: &Rel) -> bool {
        self.reason(rel).is_some()
    }

    /// The pattern responsible for ignoring `rel`, if any.
    pub fn reason(&self, rel: &Rel) -> Option<String> {
        let mut prefix = String::new();
        for (i, name) in rel.components().enumerate() {
            if i == 0 && self.root.is_match(name) {
                return Some(format!("{name} at the root of the store is reserved"));
            }
            if let Some(idx) = self.names.matches(name).first() {
                return Some(self.name_patterns[*idx].clone());
            }
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(name);
            if let Some(idx) = self.paths.matches(&prefix).first() {
                return Some(self.path_patterns[*idx].clone());
            }
        }
        None
    }
}

fn name_glob(pattern: &str) -> Result<Glob> {
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .with_context(|| format!("invalid ignore pattern {pattern:?}"))
}

fn path_glob(pattern: &str) -> Result<Glob> {
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .with_context(|| format!("invalid ignore pattern {pattern:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(s: &str) -> Rel {
        Rel::parse(s).unwrap()
    }

    fn ignore(patterns: &[&str]) -> Ignore {
        Ignore::new(&patterns.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn builtins() {
        let ig = ignore(&[]);
        assert!(ig.is_ignored(&rel(".git")));
        assert!(ig.is_ignored(&rel(".config/nvim/.git/HEAD")));
        assert!(ig.is_ignored(&rel(".DS_Store")));
        assert!(ig.is_ignored(&rel(".cubby.toml")));
        assert!(ig.is_ignored(&rel("README.md")));
        assert!(ig.is_ignored(&rel("LICENSE")));
        assert!(!ig.is_ignored(&rel("docs/README.md")));
        assert!(!ig.is_ignored(&rel(".gitconfig")));
        assert!(!ig.is_ignored(&rel(".gitignore")));
        assert!(ig.is_ignored(&rel(".config/.cubby-tmp-abc")));
    }

    #[test]
    fn name_patterns_match_any_depth() {
        let ig = ignore(&["*.swp", "lazy-lock.json", "cache/"]);
        assert!(ig.is_ignored(&rel(".vimrc.swp")));
        assert!(ig.is_ignored(&rel(".config/nvim/lazy-lock.json")));
        assert!(ig.is_ignored(&rel(".config/nvim/cache/x")));
        assert!(!ig.is_ignored(&rel(".config/nvim/init.lua")));
        assert_eq!(
            ig.reason(&rel(".config/nvim/lazy-lock.json")).as_deref(),
            Some("lazy-lock.json")
        );
    }

    #[test]
    fn path_patterns_are_anchored_to_home() {
        let ig = ignore(&[
            "~/.config/nvim/lazy-lock.json",
            ".config/**/secrets",
            "/.ssh/id_*",
        ]);
        assert!(ig.is_ignored(&rel(".config/nvim/lazy-lock.json")));
        assert!(!ig.is_ignored(&rel("other/.config/nvim/lazy-lock.json")));
        assert!(ig.is_ignored(&rel(".config/a/b/secrets/x")));
        assert!(ig.is_ignored(&rel(".config/secrets")));
        assert!(ig.is_ignored(&rel(".ssh/id_ed25519")));
        assert!(!ig.is_ignored(&rel(".ssh/config")));
        assert!(!ig.is_ignored(&rel(".config/nvim")));
    }

    #[test]
    fn invalid_pattern_is_an_error() {
        assert!(Ignore::new(&["[".to_string()]).is_err());
    }
}
