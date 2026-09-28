//! Machine-local configuration: where home is, where the store is, and where
//! cubby keeps its own state.
//!
//! Resolution order for the store: `--store` flag, `CUBBY_STORE`, the config
//! file, then the default `~/.dotfiles`.
//!
//! `CUBBY_HOME` overrides the home directory and moves the config file,
//! state directory, and default store under it. It exists so cubby can be
//! exercised against a sandbox instead of a real home directory.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::paths::{Layout, expand_tilde, expand_tilde_path, normalize};

pub const DEFAULT_STORE: &str = "~/.dotfiles";
/// How many backup sets to keep before pruning the oldest.
pub const BACKUP_SETS_TO_KEEP: usize = 20;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    store: Option<String>,
    backups: Option<bool>,
    skip: Option<Vec<String>>,
}

/// Command-line overrides that take precedence over the config file.
#[derive(Debug, Default, Clone)]
pub struct Overrides {
    pub store: Option<PathBuf>,
    pub config: Option<PathBuf>,
    pub no_backup: bool,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub layout: Layout,
    /// Keep copies of overwritten or removed files under the state directory.
    pub backups: bool,
    /// The config file that was read, or would be created by `cubby init`.
    pub config_path: PathBuf,
    /// Where history and backups live.
    pub state_dir: PathBuf,
    /// Whether the store path came from the default rather than configuration.
    pub store_is_default: bool,
    /// Patterns this machine leaves alone even though they are in the
    /// store; same syntax as the manifest's ignore patterns.
    pub skip: Vec<String>,
}

/// The directories cubby derives everything else from.
#[derive(Debug, Clone)]
pub struct Env {
    pub home: PathBuf,
    pub config_path: PathBuf,
    pub state_dir: PathBuf,
}

impl Env {
    pub fn detect() -> Result<Env> {
        if let Some(sandbox) = std::env::var_os("CUBBY_HOME") {
            let home = absolute(Path::new(&sandbox))?;
            return Ok(Env {
                config_path: home.join(".config/cubby/config.toml"),
                state_dir: home.join(".local/state/cubby"),
                home,
            });
        }
        let home = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .context("HOME is not set")?;
        let home = absolute(&home)?;
        let config_dir = xdg("XDG_CONFIG_HOME", &home, ".config");
        let state_dir = xdg("XDG_STATE_HOME", &home, ".local/state");
        Ok(Env {
            config_path: config_dir.join("cubby/config.toml"),
            state_dir: state_dir.join("cubby"),
            home,
        })
    }
}

fn xdg(var: &str, home: &Path, default: &str) -> PathBuf {
    match std::env::var_os(var) {
        Some(v) if !v.is_empty() && Path::new(&v).is_absolute() => PathBuf::from(v),
        _ => home.join(default),
    }
}

/// Make a path absolute (against the current directory) and resolve symlinks
/// when it exists.
pub fn absolute(path: &Path) -> Result<PathBuf> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let abs = normalize(&abs);
    Ok(std::fs::canonicalize(&abs).unwrap_or(abs))
}

impl Config {
    pub fn load(overrides: &Overrides) -> Result<Config> {
        let env = Env::detect()?;
        let config_path = match &overrides.config {
            Some(p) if p.is_absolute() => normalize(p),
            Some(p) => normalize(&std::env::current_dir()?.join(p)),
            None => env.config_path.clone(),
        };
        let file = read_config_file(&config_path)?;

        // Where the store setting came from decides what a relative path is
        // relative to: the current directory for the flag and the
        // environment, like any path typed in a shell; home for the config
        // file.
        let (store, from_shell, store_is_default) = if let Some(s) = &overrides.store {
            (expand_tilde_path(s, &env.home), true, false)
        } else if let Some(s) = std::env::var_os("CUBBY_STORE").filter(|s| !s.is_empty()) {
            (expand_tilde_path(Path::new(&s), &env.home), true, false)
        } else if let Some(s) = &file.store {
            (expand_tilde(s, &env.home), false, false)
        } else {
            (expand_tilde(DEFAULT_STORE, &env.home), false, true)
        };
        let store = if store.is_absolute() {
            store
        } else if from_shell {
            std::env::current_dir()?.join(store)
        } else {
            env.home.join(store)
        };
        let store = absolute(&store)?;

        if store == env.home {
            bail!("the store cannot be the home directory itself");
        }
        if env.home.starts_with(&store) {
            bail!(
                "the store ({}) cannot contain the home directory",
                store.display()
            );
        }

        Ok(Config {
            layout: Layout {
                home: env.home,
                store,
            },
            backups: !overrides.no_backup && file.backups.unwrap_or(true),
            config_path,
            state_dir: env.state_dir,
            store_is_default,
            skip: file.skip.unwrap_or_default(),
        })
    }

    /// cubby's own files, which belong to this machine and are never
    /// tracked: the config file and the state directory.
    pub fn own_paths(&self) -> Vec<(PathBuf, &'static str)> {
        vec![
            (
                self.config_path.clone(),
                "cubby's own configuration stays on this machine",
            ),
            (
                self.state_dir.clone(),
                "cubby's own state (history and backups) stays on this machine",
            ),
        ]
    }

    /// Text of a fresh config file.
    pub fn template(store: &str) -> String {
        format!(
            "# cubby configuration (this machine only)\n\
             #\n\
             # store: the directory that mirrors your home directory. Every tracked\n\
             #        file lives at the same path inside it. Version it with git.\n\
             store = {store}\n\
             \n\
             # backups: keep copies of files cubby overwrites or removes, under\n\
             #          ~/.local/state/cubby/backups (the newest {keep} runs are kept).\n\
             backups = true\n\
             \n\
             # skip: paths or patterns this machine leaves alone even though they\n\
             #       are in the store; say, macOS-only settings on a Linux machine.\n\
             #       Same syntax as ignore in the store's .cubby.toml.\n\
             #       `cubby ignore --here PATTERN` adds to it.\n\
             skip = []\n",
            store = toml_string(store),
            keep = BACKUP_SETS_TO_KEEP
        )
    }
}

/// `s` as a TOML string, escaped by TOML's rules (Rust's `{:?}` escapes are
/// not valid TOML).
pub fn toml_string(s: &str) -> String {
    toml::Value::String(s.to_owned()).to_string()
}

/// Change the list at `key` in the config file at `path` with `edit`,
/// keeping everything else in the file as it was. A missing file starts
/// from the template, pointing at `store`.
pub fn edit_list<R>(
    path: &Path,
    store: &str,
    key: &str,
    edit: impl FnOnce(&mut toml_edit::Array) -> R,
) -> Result<R> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::template(store),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("invalid config file {}", path.display()))?;
    let result = edit(crate::tomlx::array_mut(&mut doc, key)?);
    crate::fsx::write_atomic(path, doc.to_string().as_bytes())
        .with_context(|| format!("cannot write {}", path.display()))?;
    Ok(result)
}

fn read_config_file(path: &Path) -> Result<ConfigFile> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            toml::from_str(&text).with_context(|| format!("invalid config file {}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ConfigFile::default()),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses() {
        let text = Config::template("~/.dotfiles");
        let file: ConfigFile = toml::from_str(&text).unwrap();
        assert_eq!(file.store.as_deref(), Some("~/.dotfiles"));
        assert_eq!(file.backups, Some(true));
        assert_eq!(file.skip, Some(vec![]));
    }

    #[test]
    fn template_parses_with_any_store_name() {
        for store in [
            "~/dots-\u{1F469}\u{200D}\u{1F4BB}",
            "~/tab\tand \"quotes\" and \\",
            "~/esc\u{1b}",
        ] {
            let file: ConfigFile = toml::from_str(&Config::template(store)).unwrap();
            assert_eq!(file.store.as_deref(), Some(store));
        }
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(toml::from_str::<ConfigFile>("stor = \"x\"").is_err());
    }
}
