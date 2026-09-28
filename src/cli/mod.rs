//! Command-line interface.

mod diff;
mod history;
mod ignore;
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
use crate::index::Index;
use crate::lock::Lock;
use crate::manifest::Manifest;
use crate::paths::Rel;
use crate::perms;
use crate::plan::{self, Op, Plan, RunKind, Side, Skip};
use crate::scan::{Scan, Scanner, Scope};
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
    /// Ignore files on every machine, or skip them on this one
    ///
    /// A pattern without a slash matches a name at any depth (`*.swp`); one
    /// with a slash matches a path from home (`~/.config/nvim/lazy-lock.json`).
    /// With no patterns, lists what is ignored.
    Ignore {
        #[arg(value_name = "PATTERN")]
        patterns: Vec<String>,
        /// Only on this machine: add to `skip` in config.toml, not the store
        #[arg(long)]
        here: bool,
        /// Remove the patterns instead of adding them
        #[arg(long)]
        remove: bool,
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
    /// Everything this machine leaves alone: the manifest's patterns and
    /// the config file's `skip` list.
    pub ignore: Ignore,
    /// What every machine leaves alone: the manifest's patterns only. For
    /// commands about the store as a whole, like `list` and `untrack`.
    pub shared: Ignore,
    /// Colors for standard output.
    pub style: Style,
    /// Colors for standard error: warnings, errors, and prompts.
    pub estyle: Style,
    pub dry_run: bool,
    pub yes: bool,
    pub verbose: bool,
    /// Baselines and cached fingerprints for this store on this machine.
    pub index: Index,
    lock: LockState,
}

/// Whether this process may change files and the index.
enum LockState {
    Held(#[allow(dead_code)] Lock),
    /// Another cubby holds the lock.
    Busy,
    /// Let go before paging output, so other commands can run meanwhile.
    Released,
    /// The state directory cannot be written; carry on without the lock.
    Unavailable,
}

impl Ctx {
    fn load(global: &Global) -> Result<Ctx> {
        let cfg = Config::load(&Overrides {
            store: global.store.clone(),
            config: global.config.clone(),
            no_backup: global.no_backup,
        })?;
        let manifest = Manifest::load(&cfg.layout.store)?;
        let ignore = ignore_rules(&cfg, &manifest, &cfg.skip)?;
        let shared = ignore_rules(&cfg, &manifest, &[])?;
        let lock = match Lock::try_acquire(&cfg.state_dir) {
            Ok(Some(lock)) => LockState::Held(lock),
            Ok(None) => LockState::Busy,
            Err(_) => LockState::Unavailable,
        };
        let mut index = Index::load(&cfg.state_dir, &cfg.layout.store);
        if !index.existed() {
            index.import_legacy(History::new(&cfg.state_dir).synced_paths());
        }
        Ok(Ctx {
            cfg,
            manifest,
            ignore,
            shared,
            style: Style::detect(global.color),
            estyle: Style::detect_stderr(global.color),
            dry_run: global.dry_run,
            yes: global.yes,
            verbose: global.verbose,
            index,
            lock,
        })
    }

    /// Commands that change files run one at a time.
    pub fn require_lock(&self) -> Result<()> {
        if matches!(self.lock, LockState::Busy) && !self.dry_run {
            bail!(
                "another cubby is running (it holds {}); try again when it has finished",
                self.cfg.layout.pretty(&self.cfg.state_dir.join("lock"))
            );
        }
        Ok(())
    }

    /// Let other cubby processes run, for a command that is done with the
    /// index (before paging output, say).
    pub fn release_lock(&mut self) {
        self.lock = LockState::Released;
    }

    fn may_write_index(&self) -> bool {
        !self.dry_run && matches!(self.lock, LockState::Held(_) | LockState::Unavailable)
    }

    /// Remember what a scan found: baselines for paths that match and
    /// fingerprints of files that were read. Failing to write the index
    /// only costs a slower, less precise scan next time, so it is quiet.
    pub fn learn(&mut self, scan: &Scan, scope: &Scope) {
        if self.may_write_index() {
            self.index.learn(scan, scope);
            let _ = self.index.save();
        }
    }

    /// A scanner that sees what this machine syncs.
    pub fn scanner(&self) -> Scanner<'_> {
        self.scanner_with(&self.ignore)
    }

    /// A scanner that also sees what this machine skips.
    pub fn shared_scanner(&self) -> Scanner<'_> {
        self.scanner_with(&self.shared)
    }

    fn scanner_with<'a>(&'a self, ignore: &'a Ignore) -> Scanner<'a> {
        // The store, cubby's state, and its config are never walked into or
        // copied, whatever path leads to them.
        let own = std::iter::once(self.cfg.layout.store.clone())
            .chain(self.cfg.own_paths().into_iter().map(|(p, _)| p))
            .filter_map(|p| crate::fsx::lstat(&p).ok().flatten())
            .map(|m| (m.dev, m.ino))
            .collect();
        Scanner {
            layout: &self.cfg.layout,
            manifest: &self.manifest,
            ignore,
            index: &self.index,
            own,
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

    /// The scope for a command's paths (everything when there are none),
    /// and how many paths failed to resolve. `None` when paths were given
    /// and none of them resolved; the errors have been reported.
    pub fn scope_for(&self, paths: &[String]) -> Option<(Scope, usize)> {
        let (rels, failures) = self.resolve_paths(paths);
        if paths.is_empty() {
            Some((Scope::all(), failures))
        } else if rels.is_empty() {
            None
        } else {
            Some((Scope::of(rels), failures))
        }
    }

    /// Report each named path that has nothing in the store beneath it, or
    /// that is ignored. Returns how many there were.
    pub fn report_unstored(&self, scope: &Scope, scan: &Scan) -> usize {
        let mut missing = 0;
        for rel in &scope.rels {
            if let Some(reason) = self.ignore.reason(rel) {
                self.error(&format!("{rel} is ignored: {reason}"));
                missing += 1;
            } else if !scan
                .entries
                .iter()
                .any(|e| e.rel.is_within(rel) && e.store.is_some())
            {
                self.error(&format!("nothing in the store at {rel}"));
                missing += 1;
            }
        }
        missing
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
        eprintln!("{} {msg}", self.estyle.red("error:"));
    }

    pub fn warn(&self, msg: &str) {
        eprintln!("{} {msg}", self.estyle.yellow("warning:"));
    }

    pub fn note(&self, msg: &str) {
        println!("{}", self.style.dim(msg));
    }

    /// Ask for confirmation unless `--yes` was given.
    pub fn confirm(&self, question: &str) -> Result<bool> {
        if self.yes {
            return Ok(true);
        }
        ui::confirm(question, &self.estyle)
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
                Op::Overwrite | Op::Chmod => self.style.yellow("~"),
                Op::Remove => self.style.red("-"),
            };
            let path = if a.op == Op::Chmod && a.dst.is_dir() {
                format!("{}/", a.rel.as_str())
            } else {
                a.rel.as_str().to_owned()
            };
            println!("{}", ui::row(&self.style, &symbol, &path, &a.note));
        }
        for r in plan.standalone_records() {
            let path = if r.is_dir {
                format!("{}/", r.rel.as_str())
            } else {
                r.rel.as_str().to_owned()
            };
            let note = match (r.from, r.to) {
                (None, Some(to)) => format!("record permissions {}", perms::show(to)),
                (Some(from), Some(to)) => format!(
                    "record permissions {} (was {})",
                    perms::show(to),
                    perms::show(from)
                ),
                (Some(from), None) => {
                    format!("forget recorded permissions {}", perms::show(from))
                }
                (None, None) => continue,
            };
            println!(
                "{}",
                ui::row(&self.style, &self.style.yellow("~"), &path, &note)
            );
        }
    }

    /// Print what a plan skipped and why.
    pub fn print_skipped(&self, plan: &Plan) {
        let mut new_in_store = 0;
        let mut deleted_here = 0;
        let mut new = 0;
        let mut kept = Vec::new();
        for s in &plan.skipped {
            let row = |symbol: &str, note: &str| {
                println!("{}", ui::row(&self.style, symbol, s.rel.as_str(), note))
            };
            match &s.why {
                Skip::MissingAtHome { was_here: false } => new_in_store += 1,
                Skip::MissingAtHome { was_here: true } => deleted_here += 1,
                Skip::NewAtHome => new += 1,
                Skip::ChangedThere(Side::Store) => {
                    row(&self.style.dim("·"), "changed in the store; left alone");
                    kept.push(s);
                }
                Skip::ChangedThere(Side::Home) => {
                    row(&self.style.dim("·"), "changed at home; left alone");
                    kept.push(s);
                }
                Skip::DeletedFromStore => {
                    row(
                        &self.style.dim("·"),
                        "deleted from the store; not added back",
                    );
                    kept.push(s);
                }
                Skip::Conflict(text) => row(
                    &self.style.red("!"),
                    &format!("{text}; use --force to replace"),
                ),
                Skip::Error(text) => row(&self.style.red("!"), text),
            }
        }
        let is_are = |n: usize| if n == 1 { "is" } else { "are" };
        let it_them = |n: usize| if n == 1 { "it" } else { "them" };
        if new_in_store > 0 {
            self.note(&format!(
                "  {} in the store {} not at home (run `cubby restore` to bring {} over)",
                ui::plural(new_in_store, "file", "files"),
                is_are(new_in_store),
                it_them(new_in_store),
            ));
        }
        if deleted_here > 0 {
            self.note(&format!(
                "  {} deleted at home {} still in the store (`cubby untrack` drops {}, `cubby restore` brings {} back)",
                ui::plural(deleted_here, "file", "files"),
                is_are(deleted_here),
                it_them(deleted_here),
                it_them(deleted_here),
            ));
        }
        if new > 0 {
            self.note(&format!(
                "  {} at home {} not in the store yet (run `cubby` to save {})",
                ui::plural(new, "file", "files"),
                is_are(new),
                it_them(new),
            ));
        }
        if !kept.is_empty() {
            let hint = match plan.kind {
                RunKind::Restore => {
                    "`cubby` saves home's changes; `cubby restore --force PATH` replaces them"
                }
                _ => {
                    "`cubby restore` brings the store's changes home; `cubby save --force PATH` replaces them"
                }
            };
            self.note(&format!(
                "  {} changed since the last sync left alone: {hint}",
                ui::plural(kept.len(), "path", "paths")
            ));
        }
    }

    /// The line for a plan with nothing to do: `done` when that is because
    /// everything is up to date, a pointer to the skipped paths otherwise.
    pub fn print_nothing_to_do(&self, plan: &Plan, done: &str) {
        match plan.troubled() {
            0 => println!("{} {}", self.style.green("✓"), self.style.dim(done)),
            n => println!(
                "{} {}",
                self.style.red("✗"),
                self.style.dim(&format!(
                    "nothing {}; {} skipped (see above)",
                    plan.kind.past(),
                    ui::plural(n, "path", "paths")
                ))
            ),
        }
    }

    /// Carry out a plan: back up, apply, report. Returns the exit code.
    pub fn run_plan(&mut self, plan: &Plan) -> Result<i32> {
        let backup = self
            .cfg
            .backups
            .then(|| Backup::new(&self.cfg.state_dir, plan.kind.verb()));
        let history = self.history();
        let style = self.style;
        // The plan was already printed; only failures need a line of their own.
        let outcome = plan::apply(
            plan,
            &self.cfg.layout,
            backup,
            &history,
            &mut self.index,
            |action, result| {
                if let Err(msg) = result {
                    println!(
                        "{}",
                        ui::row(&style, &style.red("✗"), action.rel.as_str(), msg)
                    );
                }
            },
        )?;
        if let Err(e) = self.index.save() {
            self.warn(&format!("could not update the index: {e:#}"));
        }

        let mut summary = plan
            .kind
            .summary(outcome.done + plan.standalone_records().count());
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
        if let Some(e) = &outcome.history_error {
            self.warn(&format!("could not record this run in the history: {e}"));
        }

        if self.cfg.backups
            && let Err(e) = backup::prune(&self.cfg.state_dir, config::BACKUP_SETS_TO_KEEP)
        {
            self.warn(&format!("could not prune old backups: {e:#}"));
        }
        Ok(if outcome.failed.is_empty() { 0 } else { 1 })
    }
}

/// The ignore rules for a store: built in, the manifest's, `skip`, and
/// cubby's own files.
fn ignore_rules(cfg: &Config, manifest: &Manifest, skip: &[String]) -> Result<Ignore> {
    let mut ignore = Ignore::new(&manifest.ignore, skip)?;
    for (path, why) in cfg.own_paths() {
        if let Ok(rel) = Rel::from_path_under(&cfg.layout.home, &path) {
            ignore.reserve(rel, why);
        }
    }
    Ok(ignore)
}

/// A command whose name is a likely typo of `word` (`statu` → `status`).
/// Bare paths mean "save", so a mistyped command shows up as a missing file.
pub fn similar_command(word: &str) -> Option<String> {
    if word.contains('/') || word.starts_with(['.', '~']) {
        return None;
    }
    Cli::command()
        .get_subcommands()
        .filter(|c| !c.is_hide_set())
        .flat_map(|c| std::iter::once(c.get_name()).chain(c.get_visible_aliases()))
        .map(|name| (edit_distance(word, name), name.to_owned()))
        .filter(|(d, name)| *d > 0 && *d <= 2 && *d < name.len())
        .min()
        .map(|(_, name)| name)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur.push((prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Parse arguments, run, and return the process exit code.
pub fn run() -> i32 {
    let cli = Cli::parse();
    let color = cli.global.color;
    match dispatch(cli) {
        Ok(code) => code,
        Err(e) => {
            let style = Style::detect_stderr(color);
            eprintln!("{} {e:#}", style.red("error:"));
            1
        }
    }
}

fn dispatch(cli: Cli) -> Result<i32> {
    let global = cli.global.clone();
    // Global flags may come before a command (`cubby -n restore`); bare
    // paths and --force may not, since they belong to the implied save.
    if cli.command.is_some() && (!cli.paths.is_empty() || cli.force) {
        bail!("paths and --force go after the command, as in `cubby save --force PATH`");
    }
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
            let mut ctx = Ctx::load(&global)?;
            restore::run(&mut ctx, &paths, force)
        }
        Some(Command::Status { paths, quiet }) => {
            let mut ctx = Ctx::load(&global)?;
            status::run(&mut ctx, &paths, quiet)
        }
        Some(Command::Diff {
            paths,
            reverse,
            no_pager,
        }) => {
            let mut ctx = Ctx::load(&global)?;
            diff::run(&mut ctx, &paths, reverse, no_pager)
        }
        Some(Command::List { plain }) => {
            let ctx = Ctx::load(&global)?;
            list::run(&ctx, plain)
        }
        Some(Command::Untrack { paths }) => {
            let mut ctx = Ctx::load(&global)?;
            untrack::run(&mut ctx, &paths)
        }
        Some(Command::Ignore {
            patterns,
            here,
            remove,
        }) => {
            let mut ctx = Ctx::load(&global)?;
            ignore::run(&mut ctx, &patterns, here, remove)
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
