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
//! `README*`, and `LICENSE*`. cubby's own configuration file and state
//! directory are reserved as well, so they stay on the machine they belong to.

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
    /// Why each name pattern ignores something, by index into `names`.
    name_reasons: Vec<String>,
    path_reasons: Vec<String>,
    /// Exact paths (and everything beneath them) that are never tracked.
    reserved: Vec<(Rel, String)>,
}

impl Ignore {
    /// Build from the manifest's patterns and, for everything this machine
    /// skips, the config file's `skip` patterns.
    pub fn new(patterns: &[String], skip: &[String]) -> Result<Ignore> {
        let mut names = GlobSetBuilder::new();
        let mut paths = GlobSetBuilder::new();
        let mut name_reasons = Vec::new();
        let mut path_reasons = Vec::new();

        for p in BUILTIN_NAMES {
            names.add(glob(p)?);
            name_reasons.push(format!("{p} is never tracked"));
        }
        for (list, skipped) in [(patterns, false), (skip, true)] {
            for raw in list {
                let Some((kind, glob)) = parse(raw)? else {
                    continue;
                };
                let quoted = crate::config::toml_string(raw);
                let reason = if skipped {
                    format!("skipped on this machine (skip pattern {quoted} in config.toml)")
                } else {
                    format!("pattern {quoted} in {}", crate::manifest::FILE_NAME)
                };
                match kind {
                    Kind::Path => {
                        paths.add(glob);
                        path_reasons.push(reason);
                    }
                    Kind::Name => {
                        names.add(glob);
                        name_reasons.push(reason);
                    }
                }
            }
        }
        let mut root = GlobSetBuilder::new();
        for p in BUILTIN_ROOT {
            root.add(glob(p)?);
        }
        Ok(Ignore {
            names: names.build()?,
            paths: paths.build()?,
            root: root.build()?,
            name_reasons,
            path_reasons,
            reserved: Vec::new(),
        })
    }

    /// Never track `rel` or anything beneath it.
    pub fn reserve(&mut self, rel: Rel, why: &str) {
        self.reserved.push((rel, why.to_owned()));
    }

    /// Whether `rel` (a file or directory) is ignored. Every ancestor is
    /// checked too, so a file under an ignored directory is ignored.
    pub fn is_ignored(&self, rel: &Rel) -> bool {
        self.reason(rel).is_some()
    }

    /// Why `rel` is ignored, if it is.
    pub fn reason(&self, rel: &Rel) -> Option<String> {
        if let Some((_, why)) = self.reserved.iter().find(|(r, _)| rel.is_within(r)) {
            return Some(why.clone());
        }
        let mut prefix = String::new();
        for (i, name) in rel.components().enumerate() {
            if i == 0 && self.root.is_match(name) {
                return Some(format!("{name} at the root of the store is reserved"));
            }
            if let Some(idx) = self.names.matches(name).first() {
                return Some(self.name_reasons[*idx].clone());
            }
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(name);
            if let Some(idx) = self.paths.matches(&prefix).first() {
                return Some(self.path_reasons[*idx].clone());
            }
        }
        None
    }
}

enum Kind {
    /// Matches a name at any depth.
    Name,
    /// Matches a path relative to home.
    Path,
}

/// What kind of pattern `raw` is, compiled; `None` for blank lines and
/// comments.
fn parse(raw: &str) -> Result<Option<(Kind, Glob)>> {
    let p = raw.trim();
    if p.is_empty() || p.starts_with('#') {
        return Ok(None);
    }
    // `~/bin` and `/bin` are anchored at home even without another slash.
    let anchored = p.starts_with("~/") || p.starts_with('/');
    let stripped = p
        .strip_prefix("~/")
        .or_else(|| p.strip_prefix('/'))
        .unwrap_or(p);
    let stripped = stripped.strip_suffix('/').unwrap_or(stripped);
    if anchored || stripped.contains('/') {
        Ok(Some((Kind::Path, glob(stripped)?)))
    } else {
        Ok(Some((Kind::Name, glob(stripped)?)))
    }
}

/// Check that `raw` is a pattern cubby can use.
pub fn validate(raw: &str) -> Result<()> {
    parse(raw).map(|_| ())
}

fn glob(pattern: &str) -> Result<Glob> {
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

    fn strings(patterns: &[&str]) -> Vec<String> {
        patterns.iter().map(|s| s.to_string()).collect()
    }

    fn ignore(patterns: &[&str]) -> Ignore {
        Ignore::new(&strings(patterns), &[]).unwrap()
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
        assert_eq!(
            ig.reason(&rel(".config/nvim/.git/HEAD")).as_deref(),
            Some(".git is never tracked")
        );
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
            Some("pattern \"lazy-lock.json\" in .cubby.toml")
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
    fn a_leading_tilde_or_slash_anchors_a_name_at_home() {
        let ig = ignore(&["~/bin", "/.cache", "tmp/"]);
        assert!(ig.is_ignored(&rel("bin/tool")));
        assert!(!ig.is_ignored(&rel(".config/tool/bin/helper")));
        assert!(ig.is_ignored(&rel(".cache/x")));
        assert!(!ig.is_ignored(&rel(".local/.cache/x")));
        // A trailing slash alone does not anchor.
        assert!(ig.is_ignored(&rel(".config/app/tmp/x")));
    }

    #[test]
    fn reserved_paths_cover_their_contents() {
        let mut ig = ignore(&[]);
        ig.reserve(rel(".local/state/cubby"), "cubby's state stays here");
        assert!(ig.is_ignored(&rel(".local/state/cubby")));
        assert!(ig.is_ignored(&rel(".local/state/cubby/history.log")));
        assert!(!ig.is_ignored(&rel(".local/state/cubbyx")));
        assert!(!ig.is_ignored(&rel(".local/state")));
        assert_eq!(
            ig.reason(&rel(".local/state/cubby/x")).as_deref(),
            Some("cubby's state stays here")
        );
    }

    #[test]
    fn skip_patterns_say_where_they_come_from() {
        let ig = Ignore::new(&strings(&["*.swp"]), &strings(&["~/.config/aerospace"])).unwrap();
        assert_eq!(
            ig.reason(&rel(".config/aerospace/aerospace.toml"))
                .as_deref(),
            Some("skipped on this machine (skip pattern \"~/.config/aerospace\" in config.toml)")
        );
        assert!(ig.is_ignored(&rel("x.swp")));
    }

    #[test]
    fn invalid_pattern_is_an_error() {
        assert!(Ignore::new(&strings(&["["]), &[]).is_err());
        assert!(Ignore::new(&[], &strings(&["["])).is_err());
        assert!(validate("[").is_err());
        assert!(validate("*.swp").is_ok());
    }
}
