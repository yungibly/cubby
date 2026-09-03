use std::path::Path;

use anyhow::{Context, Result, bail};

use super::Global;
use crate::config::{self, Config, Env};
use crate::manifest::Manifest;
use crate::paths::{expand_tilde, normalize};
use crate::ui::Style;

pub fn run(global: &Global, dir: Option<&str>, force: bool) -> Result<i32> {
    let style = Style::detect(global.color);
    let env = Env::detect()?;
    let config_path = global.config.clone().unwrap_or(env.config_path.clone());

    // The store: the argument, the --store flag, or the default.
    let store_text = match (dir, &global.store) {
        (Some(d), _) => d.to_owned(),
        (None, Some(s)) => s.display().to_string(),
        (None, None) => config::DEFAULT_STORE.to_owned(),
    };
    let store = expand_tilde(&store_text, &env.home);
    let store = if store.is_absolute() {
        store
    } else {
        std::env::current_dir()?.join(store)
    };
    let store = normalize(&store);
    if store == env.home {
        bail!("the store cannot be the home directory itself");
    }
    if env.home.starts_with(&store) {
        bail!("the store cannot contain the home directory");
    }

    // Write the store path the way the user thinks of it.
    let pretty: String = match store.strip_prefix(&env.home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => store.display().to_string(),
    };

    if config_path.exists() && !force {
        let existing = Config::load(&config::Overrides {
            store: None,
            config: Some(config_path.clone()),
            no_backup: false,
        })?;
        if existing.layout.store != store {
            bail!(
                "{} already exists and points at {}; pass --force to point it at {pretty} instead",
                display(&config_path, &env.home),
                display(&existing.layout.store, &env.home)
            );
        }
        println!(
            "{} {}",
            style.green("✓"),
            style.dim(&format!(
                "config already at {}",
                display(&config_path, &env.home)
            ))
        );
    } else {
        crate::fsx::write_atomic(&config_path, Config::template(&pretty).as_bytes())
            .with_context(|| format!("cannot write {}", config_path.display()))?;
        println!(
            "{} wrote {}",
            style.green("✓"),
            display(&config_path, &env.home)
        );
    }

    if store.is_dir() {
        println!(
            "{} {}",
            style.green("✓"),
            style.dim(&format!("store already at {pretty}"))
        );
    } else {
        std::fs::create_dir_all(&store)
            .with_context(|| format!("cannot create {}", store.display()))?;
        println!("{} created {pretty}", style.green("✓"));
    }
    let manifest_path = Manifest::path(&store);
    if manifest_path.exists() {
        println!(
            "{} {}",
            style.green("✓"),
            style.dim(&format!(
                "manifest already at {pretty}/{}",
                crate::manifest::FILE_NAME
            ))
        );
    } else {
        Manifest::fresh().save(&store)?;
        println!(
            "{} wrote {pretty}/{}",
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
                format!("git -C {pretty} init")
            ))
        );
    }
    Ok(0)
}

fn display(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}
