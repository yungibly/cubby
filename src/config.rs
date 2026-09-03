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

use crate::paths::{Layout, expand_tilde, normalize};

pub const DEFAULT_STORE: &str = "~/.dotfiles";
/// How many backup sets to keep before pruning the oldest.
pub const BACKUP_SETS_TO_KEEP: usize = 20;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    store: Option<String>,
    backups: Option<bool>,
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
fn absolute(path: &Path) -> Result<PathBuf> {
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
        let config_path = overrides.config.clone().unwrap_or(env.config_path.clone());
        let file = read_config_file(&config_path)?;

        let (store, store_is_default) = if let Some(s) = &overrides.store {
            (s.clone(), false)
        } else if let Some(s) = std::env::var_os("CUBBY_STORE").filter(|s| !s.is_empty()) {
            (PathBuf::from(s), false)
        } else if let Some(s) = &file.store {
            (expand_tilde(s, &env.home), false)
        } else {
            (expand_tilde(DEFAULT_STORE, &env.home), true)
        };
        // A relative store in the config file is relative to home; on the
        // command line it is relative to the current directory, like any path.
        let store = if store.is_absolute() {
            store
        } else if overrides.store.is_some() || std::env::var_os("CUBBY_STORE").is_some() {
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
        })
    }

    /// Text of a fresh config file.
    pub fn template(store: &str) -> String {
        format!(
            "# cubby configuration (this machine only)\n\
             #\n\
             # store: the directory that mirrors your home directory. Every tracked\n\
             #        file lives at the same path inside it. Version it with git.\n\
             store = {store:?}\n\
             \n\
             # backups: keep copies of files cubby overwrites or removes, under\n\
             #          ~/.local/state/cubby/backups (the newest {keep} runs are kept).\n\
             backups = true\n",
            keep = BACKUP_SETS_TO_KEEP
        )
    }
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
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(toml::from_str::<ConfigFile>("stor = \"x\"").is_err());
    }
}
