//! Command-line interface.

mod diff;
mod history;
mod init;
mod list;
mod restore;
mod save;
mod status;
mod untrack;

use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{Args, CommandFactory, Parser, Subcommand};

use crate::backup::{self, Backup};
use crate::config::{self, Config, Overrides};
use crate::history::History;
use crate::ignore::Ignore;
use crate::manifest::Manifest;
use crate::paths::Rel;
use crate::plan::{self, Op, Plan, Skip};
use crate::scan::Scanner;
use crate::ui::{self, ColorChoice, Style};

const LONG_ABOUT: &str = "\
cubby keeps copies of your dotfiles in a store: a directory that mirrors
your home directory, one file at the same relative path for every file you
track. Version the store with git and you have your dotfiles everywhere.

  cubby ~/.zshrc ~/.config/nvim   start tracking (copies home → store)
  cubby                           save every tracked file that changed
  cubby restore                   copy the store back over home
  cubby status                    see what differs

Directories are tracked as a whole: new files under them are picked up and
files you delete at home are removed from the store. Files are only ever
copied, never linked. Anything cubby overwrites or removes is backed up
first.";

#[derive(Parser)]
#[command(
    name = "cubby",
    version,
    about = "Keep copies of your dotfiles in a store that mirrors your home directory",
    long_about = LONG_ABOUT,
    args_conflicts_with_subcommands = true,
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Files or directories to save (same as `cubby save PATH...`)
    #[arg(value_name = "PATH", value_hint = clap::ValueHint::AnyPath)]
    paths: Vec<String>,

    /// Replace files whose kind differs between home and store
    #[arg(long)]
    force: bool,

    #[command(flatten)]
    global: Global,
}

#[derive(Args, Clone)]
struct Global {
    /// Show what would happen without changing anything
    #[arg(short = 'n', long, global = true)]
    dry_run: bool,

    /// Skip confirmation prompts
    #[arg(short = 'y', long, global = true)]
    yes: bool,

    /// Show every file, including unchanged ones
    #[arg(short = 'v', long, global = true)]
    verbose: bool,

    /// Use this store instead of the configured one
    #[arg(long, global = true, value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
    store: Option<PathBuf>,

    /// Read this config file instead of the default
    #[arg(long, global = true, value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    config: Option<PathBuf>,

    /// Do not back up files that get overwritten or removed
    #[arg(long, global = true)]
    no_backup: bool,

    /// When to use colors
    #[arg(long, global = true, value_enum, default_value_t = ColorChoice::Auto, value_name = "WHEN")]
    color: ColorChoice,
}

#[derive(Subcommand)]
enum Command {
    /// Copy files from home into the store (all tracked files, or the given paths)
    #[command(visible_alias = "add")]
    Save {
        /// Files or directories to save; a directory becomes tracked as a whole
        #[arg(value_name = "PATH", value_hint = clap::ValueHint::AnyPath)]
        paths: Vec<String>,
        /// Replace files whose kind differs between home and store
        #[arg(long)]
        force: bool,
    },
    /// Copy files from the store back into home (all tracked files, or the given paths)
    Restore {
        #[arg(value_name = "PATH", value_hint = clap::ValueHint::AnyPath)]
        paths: Vec<String>,
        /// Replace files whose kind differs between home and store
        #[arg(long)]
        force: bool,
    },
    /// Show what differs between home and the store
    Status {
        #[arg(value_name = "PATH", value_hint = clap::ValueHint::AnyPath)]
        paths: Vec<String>,
        /// Print nothing; exit 0 when up to date, 1 when anything differs, 2 on error
        #[arg(short, long)]
        quiet: bool,
    },
    /// Show line-by-line differences between home and the store
    Diff {
        #[arg(value_name = "PATH", value_hint = clap::ValueHint::AnyPath)]
        paths: Vec<String>,
        /// Diff in the restore direction (store as new, home as old)
        #[arg(short = 'R', long)]
        reverse: bool,
        /// Print directly instead of through the pager
        #[arg(long)]
        no_pager: bool,
    },
    /// Show everything in the store as a tree
    #[command(visible_alias = "ls")]
    List {
        /// One path per line, for scripts
        #[arg(short, long)]
        plain: bool,
    },
    /// Stop tracking files or directories (removes them from the store)
    #[command(visible_alias = "rm")]
    Untrack {
        #[arg(value_name = "PATH", required = true, value_hint = clap::ValueHint::AnyPath)]
        paths: Vec<String>,
    },
    /// Show what cubby has done
    History {
        /// How many entries to show
        #[arg(short = 'c', long, default_value_t = 20, value_name = "N")]
        count: usize,
        /// Show all entries
        #[arg(short, long)]
        all: bool,
        /// Only show this kind of operation
        #[arg(long, value_name = "OP")]
        op: Option<String>,
    },
    /// Create the config file and the store
    Init {
        /// Where the store should live (default: ~/.dotfiles)
        #[arg(value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
        dir: Option<String>,
        /// Overwrite an existing config file
        #[arg(long)]
        force: bool,
    },
    /// Print a shell completion script
    #[command(hide = true)]
    Completion {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

/// Everything a command needs.
pub struct Ctx {
    pub cfg: Config,
    pub manifest: Manifest,
    pub ignore: Ignore,
    pub style: Style,
    pub dry_run: bool,
    pub yes: bool,
    pub verbose: bool,
}

impl Ctx {
    fn load(global: &Global) -> Result<Ctx> {
        let cfg = Config::load(&Overrides {
            store: global.store.clone(),
            config: global.config.clone(),
            no_backup: global.no_backup,
        })?;
        let manifest = Manifest::load(&cfg.layout.store)?;
        let ignore = Ignore::new(&manifest.ignore)?;
        Ok(Ctx {
            cfg,
            manifest,
            ignore,
            style: Style::detect(global.color),
            dry_run: global.dry_run,
            yes: global.yes,
            verbose: global.verbose,
        })
    }

    pub fn scanner(&self) -> Scanner<'_> {
        Scanner {
            layout: &self.cfg.layout,
            manifest: &self.manifest,
            ignore: &self.ignore,
        }
    }

    pub fn history(&self) -> History {
        History::new(&self.cfg.state_dir)
    }

    pub fn store_label(&self) -> String {
        self.cfg.layout.pretty(&self.cfg.layout.store)
    }

    /// Fail with a helpful message when the store does not exist yet.
    pub fn require_store(&self) -> Result<()> {
        if self.cfg.layout.store.is_dir() {
            return Ok(());
        }
        let store = self.store_label();
        let config = self.cfg.layout.pretty(&self.cfg.config_path);
        if self.cfg.store_is_default {
            bail!(
                "no store at {store} (the default; no config at {config}). Run `cubby init` to create one there, or `cubby init DIR` to use another directory"
            );
        }
        bail!(
            "store {store} (from {config}) does not exist. Create it with `cubby init {store}`, or clone your dotfiles there"
        );
    }

    /// Resolve command-line paths, reporting each failure and returning the
    /// ones that resolved.
    pub fn resolve_paths(&self, paths: &[String]) -> (Vec<Rel>, usize) {
        let mut rels = Vec::new();
        let mut failures = 0;
        for p in paths {
            match self.cfg.layout.resolve(p) {
                Ok(rel) => {
                    if !rels.contains(&rel) {
                        rels.push(rel);
                    }
                }
                Err(e) => {
                    self.error(&format!("{e:#}"));
                    failures += 1;
                }
            }
        }
        (rels, failures)
    }

    pub fn error(&self, msg: &str) {
        eprintln!("{} {msg}", self.style.red("error:"));
    }

    pub fn warn(&self, msg: &str) {
        eprintln!("{} {msg}", self.style.yellow("warning:"));
    }

    pub fn note(&self, msg: &str) {
        println!("{}", self.style.dim(msg));
    }

    /// Ask for confirmation unless `--yes` was given.
    pub fn confirm(&self, question: &str) -> Result<bool> {
        if self.yes {
            return Ok(true);
        }
        ui::confirm(question, &self.style)
    }

    /// Print the actions of a plan, grouped and capped unless verbose.
    pub fn print_plan(&self, plan: &Plan) {
        let cap = if self.verbose { usize::MAX } else { 40 };
        for (i, a) in plan.actions.iter().enumerate() {
            if i == cap {
                println!(
                    "  {}",
                    self.style.dim(&format!(
                        "… and {} more (use --verbose to list all)",
                        plan.actions.len() - cap
                    ))
                );
                break;
            }
            let symbol = match a.op {
                Op::Create => self.style.green("+"),
                Op::Overwrite => self.style.yellow("~"),
                Op::Remove => self.style.red("-"),
            };
            println!("{}", ui::row(&self.style, &symbol, a.rel.as_str(), &a.note));
        }
    }

    /// Print what a plan skipped and why.
    pub fn print_skipped(&self, plan: &Plan) {
        let mut missing = 0;
        let mut new = 0;
        for s in &plan.skipped {
            match &s.why {
                Skip::MissingAtHome => missing += 1,
                Skip::NewAtHome => new += 1,
                Skip::Conflict(text) => {
                    println!(
                        "{}",
                        ui::row(
                            &self.style,
                            &self.style.red("!"),
                            s.rel.as_str(),
                            &format!("{text}; use --force to replace")
                        )
                    );
                }
                Skip::Error(text) => {
                    println!(
                        "{}",
                        ui::row(&self.style, &self.style.red("!"), s.rel.as_str(), text)
                    );
                }
            }
        }
        if missing > 0 {
            self.note(&format!(
                "  {} in the store {} not at home (run `cubby restore` to bring {} back)",
                ui::plural(missing, "file", "files"),
                if missing == 1 { "is" } else { "are" },
                if missing == 1 { "it" } else { "them" },
            ));
        }
        if new > 0 {
            self.note(&format!(
                "  {} at home {} not in the store yet (run `cubby` to save {})",
                ui::plural(new, "file", "files"),
                if new == 1 { "is" } else { "are" },
                if new == 1 { "it" } else { "them" },
            ));
        }
    }

    /// Carry out a plan: back up, apply, report. Returns the exit code.
    pub fn run_plan(&self, plan: &Plan) -> Result<i32> {
        let backup = self
            .cfg
            .backups
            .then(|| Backup::new(&self.cfg.state_dir, plan.direction.verb()));
        let history = self.history();
        // The plan was already printed; only failures need a line of their own.
        let outcome = plan::apply(
            plan,
            &self.cfg.layout,
            backup,
            &history,
            |action, result| {
                if let Err(msg) = result {
                    println!(
                        "{}",
                        ui::row(&self.style, &self.style.red("✗"), action.rel.as_str(), msg)
                    );
                }
            },
        )?;

        let mut summary = format!(
            "{} {}",
            plan.direction.past(),
            ui::plural(outcome.done, "change", "changes")
        );
        if !outcome.failed.is_empty() {
            summary.push_str(&format!(", {} failed", outcome.failed.len()));
        }
        if let Some(dir) = &outcome.backup_dir {
            summary.push_str(&format!(
                " · {} backed up to {}",
                ui::plural(outcome.backed_up, "file", "files"),
                self.cfg.layout.pretty(dir)
            ));
        }
        println!("{}", self.style.dim(&summary));

        if self.cfg.backups
            && let Err(e) = backup::prune(&self.cfg.state_dir, config::BACKUP_SETS_TO_KEEP)
        {
            self.warn(&format!("could not prune old backups: {e:#}"));
        }
        Ok(if outcome.failed.is_empty() { 0 } else { 1 })
    }
}

/// Parse arguments, run, and return the process exit code.
pub fn run() -> i32 {
    let cli = Cli::parse();
    match dispatch(cli) {
        Ok(code) => code,
        Err(e) => {
            let style = Style::detect(ColorChoice::Auto);
            eprintln!("{} {e:#}", style.red("error:"));
            1
        }
    }
}

fn dispatch(cli: Cli) -> Result<i32> {
    let global = cli.global.clone();
    match cli.command {
        None => {
            let mut ctx = Ctx::load(&global)?;
            save::run(&mut ctx, &cli.paths, cli.force)
        }
        Some(Command::Save { paths, force }) => {
            let mut ctx = Ctx::load(&global)?;
            save::run(&mut ctx, &paths, force)
        }
        Some(Command::Restore { paths, force }) => {
            let ctx = Ctx::load(&global)?;
            restore::run(&ctx, &paths, force)
        }
        Some(Command::Status { paths, quiet }) => {
            let ctx = Ctx::load(&global)?;
            status::run(&ctx, &paths, quiet)
        }
        Some(Command::Diff {
            paths,
            reverse,
            no_pager,
        }) => {
            let ctx = Ctx::load(&global)?;
            diff::run(&ctx, &paths, reverse, no_pager)
        }
        Some(Command::List { plain }) => {
            let ctx = Ctx::load(&global)?;
            list::run(&ctx, plain)
        }
        Some(Command::Untrack { paths }) => {
            let mut ctx = Ctx::load(&global)?;
            untrack::run(&mut ctx, &paths)
        }
        Some(Command::History { count, all, op }) => {
            let ctx = Ctx::load(&global)?;
            history::run(&ctx, count, all, op.as_deref())
        }
        Some(Command::Init { dir, force }) => init::run(&global, dir.as_deref(), force),
        Some(Command::Completion { shell }) => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "cubby", &mut std::io::stdout());
            Ok(0)
        }
    }
}
