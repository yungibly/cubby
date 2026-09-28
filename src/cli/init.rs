use std::path::Path;

use anyhow::{Context, Result, bail};

use super::Global;
use crate::config::{self, Config, Env};
use crate::manifest::Manifest;
use crate::paths::{expand_tilde, expand_tilde_path, normalize, pretty};
use crate::ui::Style;

pub fn run(global: &Global, dir: Option<&str>, force: bool) -> Result<i32> {
    let style = Style::detect(global.color);
    let env = Env::detect()?;
    let config_path = match &global.config {
        Some(p) if p.is_absolute() => normalize(p),
        Some(p) => normalize(&std::env::current_dir()?.join(p)),
        None => env.config_path.clone(),
    };
    let display = |p: &Path| pretty(p, &env.home);

    // The store: the argument, the --store flag, or the default.
    let store = match (dir, &global.store) {
        (Some(d), _) => expand_tilde(d, &env.home),
        (None, Some(s)) => expand_tilde_path(s, &env.home),
        (None, None) => expand_tilde(config::DEFAULT_STORE, &env.home),
    };
    let store = if store.is_absolute() {
        store
    } else {
        std::env::current_dir()?.join(store)
    };
    let store = normalize(&store);
    // Where the store really is: one reached through a symlink is still the
    // same store.
    let real = config::absolute(&store)?;
    if real == env.home {
        bail!("the store cannot be the home directory itself");
    }
    if env.home.starts_with(&real) {
        bail!("the store cannot contain the home directory");
    }

    // Write the store path the way the user thinks of it.
    let typed = display(&store);

    if config_path.exists() && !force {
        let existing = Config::load(&config::Overrides {
            store: None,
            config: Some(config_path.clone()),
            no_backup: false,
        })?;
        if existing.layout.store != real {
            bail!(
                "{} already exists and points at {}; pass --force to point it at {typed} instead",
                display(&config_path),
                display(&existing.layout.store)
            );
        }
        println!(
            "{} {}",
            style.green("✓"),
            style.dim(&format!("config already at {}", display(&config_path)))
        );
    } else {
        crate::fsx::write_atomic(&config_path, Config::template(&typed).as_bytes())
            .with_context(|| format!("cannot write {}", config_path.display()))?;
        println!("{} wrote {}", style.green("✓"), display(&config_path));
    }

    if store.is_dir() {
        println!(
            "{} {}",
            style.green("✓"),
            style.dim(&format!("store already at {typed}"))
        );
    } else {
        std::fs::create_dir_all(&store)
            .with_context(|| format!("cannot create {}", store.display()))?;
        println!("{} created {typed}", style.green("✓"));
    }
    let manifest_path = Manifest::path(&store);
    if manifest_path.exists() {
        println!(
            "{} {}",
            style.green("✓"),
            style.dim(&format!(
                "manifest already at {typed}/{}",
                crate::manifest::FILE_NAME
            ))
        );
    } else {
        Manifest::fresh().save(&store)?;
        println!(
            "{} wrote {typed}/{}",
            style.green("✓"),
            crate::manifest::FILE_NAME
        );
    }

    println!();
    println!("{}", style.dim("next:"));
    println!(
        "{}",
        style.dim(&format!(
            "  {:<32} start tracking files or directories",
            "cubby ~/.zshrc ~/.config/nvim"
        ))
    );
    if !store.join(".git").exists() {
        println!(
            "{}",
            style.dim(&format!(
                "  {:<32} version the store (recommended)",
                format!("git -C {typed} init")
            ))
        );
    }
    Ok(0)
}
