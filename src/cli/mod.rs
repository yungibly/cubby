//! Command-line interface.

mod backups;
mod diff;
mod history;
mod ignore;
mod init;
mod list;
mod restore;
mod save;
mod status;
mod sync;
mod undo;
mod untrack;

use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{Args, CommandFactory, Parser, Subcommand};

use crate::config::{Config, Overrides};
use crate::ignore::Ignore;
use crate::index::Index;
use crate::lock::Lock;
use crate::manifest::Manifest;
use crate::paths::Rel;
use crate::perms;
use crate::plan::{self, Op, Plan, RunKind, Side, Skip};
use crate::runs::{self, Run};
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

    /// Save files that look like secrets without asking
    #[arg(long)]
    allow_secrets: bool,

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
        /// Save files that look like secrets without asking
        #[arg(long)]
        allow_secrets: bool,
    },
    /// Copy files from the store back into home (all tracked files, or the given paths)
    Restore {
        #[arg(value_name = "PATH", value_hint = clap::ValueHint::AnyPath)]
        paths: Vec<String>,
        /// Replace files whose kind differs between home and store
        #[arg(long)]
        force: bool,
    },
    /// Save what changed at home and restore what changed in the store
    ///
    /// Each path is copied the way it changed since the last sync. Paths
    /// changed on both sides are left for you to decide; nothing at home is
    /// ever deleted.
    Sync {
        #[arg(value_name = "PATH", value_hint = clap::ValueHint::AnyPath)]
        paths: Vec<String>,
        /// Save files that look like secrets without asking
        #[arg(long)]
        allow_secrets: bool,
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
    /// Show what cubby has done, one line per run
    History {
        /// How many runs to show
        #[arg(short = 'c', long, default_value_t = 20, value_name = "N")]
        count: usize,
        /// Show every run
        #[arg(short, long)]
        all: bool,
        /// Only show runs of this kind
        #[arg(long, value_name = "KIND", value_parser = ["save", "restore", "sync", "untrack", "undo"])]
        op: Option<String>,
    },
    /// Reverse the last run, or the run with this id (see `cubby history`)
    ///
    /// Paths that changed again since that run are left alone.
    Undo {
        #[arg(value_name = "RUN")]
        id: Option<String>,
    },
    /// List the backups of overwritten and removed files, or one path's
    Backups {
        #[arg(value_name = "PATH", value_hint = clap::ValueHint::AnyPath)]
        path: Option<String>,
    },
    /// Create the config file and the store, or clone a store
    ///
    /// `cubby init URL [DIR]` clones a store (from github.com, say) for a
    /// new machine and shows what `cubby restore` would copy.
    Init {
        /// A repository to clone, or where the store should live
        /// (default: ~/.dotfiles)
        #[arg(value_name = "URL|DIR", value_hint = clap::ValueHint::DirPath)]
        target: Option<String>,
        /// Where to clone to, after a URL
        #[arg(value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
        dir: Option<String>,
        /// Overwrite an existing config file
        #[arg(long)]
        force: bool,
    },
    /// Run git in the store: `cubby git status`, `cubby git push`
    #[command(disable_help_flag = true)]
    Git {
        #[arg(
            value_name = "ARGS",
            trailing_var_arg = true,
            allow_hyphen_values = true
        )]
        args: Vec<std::ffi::OsString>,
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
            index.import_legacy(runs::legacy_synced(&cfg.state_dir));
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
        let mut conflicts = 0;
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
                Skip::Conflict(text) if plan.kind == RunKind::Sync => {
                    row(&self.style.red("!"), text);
                    conflicts += 1;
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
        if conflicts > 0 {
            self.note(&format!(
                "  {} left for you: `cubby diff PATH` shows both sides, `cubby save --force PATH` or `cubby restore --force PATH` picks one",
                ui::plural(conflicts, "path", "paths")
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

    /// Mark the files a plan would put in the store for the first time that
    /// look like secrets, and return their paths.
    pub fn mark_secrets(&self, plan: &mut Plan) -> Vec<Rel> {
        let mut found = Vec::new();
        for a in &mut plan.actions {
            if a.side != Side::Store || a.op != Op::Create {
                continue;
            }
            let meta = a
                .src
                .as_deref()
                .and_then(|p| crate::fsx::lstat(p).ok().flatten());
            if let Some(why) = meta.as_ref().and_then(crate::secrets::check) {
                a.note.push_str(&format!("; looks secret: {why}"));
                found.push(a.rel.clone());
            }
        }
        found
    }

    /// Warn about files that look like secrets, before the plan's question.
    pub fn warn_secrets(&self, secrets: &[Rel]) {
        if !secrets.is_empty() {
            self.warn(&format!(
                "{} above {}; the store usually ends up in git, so check before pushing it anywhere public",
                ui::plural(secrets.len(), "file", "files"),
                if secrets.len() == 1 {
                    "looks like a secret"
                } else {
                    "look like secrets"
                }
            ));
        }
    }

    /// Ask separately about files that look like secrets: `--yes` alone
    /// does not answer this, `--allow-secrets` does. Those not allowed are
    /// taken out of the plan. Returns how many were.
    pub fn withhold_secrets(&self, plan: &mut Plan, secrets: &[Rel], allow: bool) -> Result<usize> {
        if secrets.is_empty() || allow {
            return Ok(0);
        }
        let keep = !self.yes
            && ui::confirm(
                &format!(
                    "save the {} too?",
                    if secrets.len() == 1 {
                        "file that looks like a secret".to_owned()
                    } else {
                        format!("{} files that look like secrets", secrets.len())
                    }
                ),
                &self.estyle,
            )?;
        if keep {
            return Ok(0);
        }
        plan.actions.retain(|a| !secrets.contains(&a.rel));
        plan.records.retain(|r| !secrets.contains(&r.rel));
        self.note(&format!(
            "left out {}; `--allow-secrets` saves {}",
            ui::plural(
                secrets.len(),
                "file that looks secret",
                "files that look secret"
            ),
            if secrets.len() == 1 { "it" } else { "them" }
        ));
        Ok(secrets.len())
    }

    /// After saving: warn about saved files git will never commit, and
    /// about files that git reads as settings for the store repository.
    pub fn warn_git(&self, plan: &Plan) {
        let saved: Vec<Rel> = plan
            .actions
            .iter()
            .filter(|a| a.side == Side::Store && matches!(a.op, Op::Create | Op::Overwrite))
            .map(|a| a.rel.clone())
            .collect();
        for rel in saved.iter().filter(|r| crate::git::is_repo_setting(r)) {
            self.warn(&format!(
                "{rel} sits at the root of the store, so git also applies it to the store's own repository"
            ));
        }
        let ignored = crate::git::ignored(&self.cfg.layout.store, &saved);
        if !ignored.is_empty() {
            self.warn(&format!(
                "git ignores {}, so {} never be committed or reach another machine:",
                ui::plural(ignored.len(), "file just saved", "files just saved"),
                if ignored.len() == 1 {
                    "it will"
                } else {
                    "they will"
                }
            ));
            for i in &ignored {
                eprintln!("  {}  {}", i.rel.as_str(), self.estyle.dim(&i.rule));
            }
        }
    }

    /// One line about the store's repository: changes to commit, commits
    /// to push or pull. Forgetting those is how dotfiles fail to follow you.
    pub fn print_git_state(&self) {
        let store = &self.cfg.layout.store;
        let Some(g) = crate::git::state(store) else {
            if !store.join(".git").exists() {
                // Git missing, or the store not in a repository (or in
                // one that ignores it).
                println!(
                    "{}",
                    self.style
                        .dim("store: not a git repository; `cubby git init` versions it")
                );
            }
            return;
        };
        let mut parts = Vec::new();
        if g.changes > 0 {
            parts.push(format!(
                "{} to commit",
                ui::plural(g.changes, "change", "changes")
            ));
        }
        if g.ahead > 0 {
            parts.push(format!(
                "{} to push",
                ui::plural(g.ahead as usize, "commit", "commits")
            ));
        }
        if g.behind > 0 {
            parts.push(format!(
                "{} to pull",
                ui::plural(g.behind as usize, "commit", "commits")
            ));
        }
        let line = match (parts.is_empty(), &g.upstream) {
            (true, Some(up)) => format!("store: committed, up to date with {up}"),
            (true, None) => "store: committed (no upstream to push to)".to_owned(),
            (false, _) => format!("store: {}", parts.join(" · ")),
        };
        println!("{}", self.style.dim(&line));
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

    /// Carry out a plan as one run: record it, keep copies of what it
    /// overwrites or removes, apply it, and report. Manifest edits made
    /// while planning are written and recorded as part of the run, so
    /// `cubby undo` reverses them too. Returns the exit code.
    pub fn run_plan(&mut self, plan: &Plan) -> Result<i32> {
        let mut run = Run::start(
            &self.cfg.state_dir,
            plan.kind.verb(),
            &self.cfg.layout.store,
            self.cfg.backups,
        );
        if let Some(id) = &plan.undoes {
            run.set_undoes(id);
        }
        if !self.manifest.edits().is_empty() {
            run.record_manifest(self.manifest.edits());
            self.manifest.save(&self.cfg.layout.store)?;
        }
        let style = self.style;
        // The plan was already printed; only failures need a line of their own.
        let outcome = plan::apply(
            plan,
            &self.cfg.layout,
            &mut run,
            &mut self.index,
            |action, result| {
                if let Err(msg) = result {
                    println!(
                        "{}",
                        ui::row(&style, &style.red("✗"), action.rel.as_str(), msg)
                    );
                }
            },
        );
        if let Err(e) = self.index.save() {
            self.warn(&format!("could not update the index: {e:#}"));
        }
        let backed_up = run.backed_up();
        let id = run.id().to_owned();
        let recorded = run.finish();

        let mut summary = plan
            .kind
            .summary(outcome.done + plan.standalone_records().count());
        if !outcome.failed.is_empty() {
            summary.push_str(&format!(", {} failed", outcome.failed.len()));
        }
        if backed_up > 0 {
            summary.push_str(&format!(
                " · {} backed up",
                ui::plural(backed_up, "file", "files")
            ));
        }
        if recorded.is_ok() && outcome.done > 0 {
            // A plain `cubby undo` steps back past undos, so reversing one
            // takes its id.
            if plan.kind == RunKind::Undo {
                summary.push_str(&format!(" · `cubby undo {id}` reverses this"));
            } else {
                summary.push_str(" · `cubby undo` reverses this");
            }
        }
        println!("{}", self.style.dim(&summary));
        if let Err(e) = recorded {
            self.warn(&format!(
                "could not record this run, so it cannot be undone: {e:#}"
            ));
        }
        if let Err(e) = runs::prune(&self.cfg.state_dir, self.cfg.backup_days) {
            self.warn(&format!("could not remove old backups: {e:#}"));
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
    if cli.command.is_some() && (!cli.paths.is_empty() || cli.force || cli.allow_secrets) {
        bail!("paths and --force go after the command, as in `cubby save --force PATH`");
    }
    match cli.command {
        None => {
            let mut ctx = Ctx::load(&global)?;
            save::run(&mut ctx, &cli.paths, cli.force, cli.allow_secrets)
        }
        Some(Command::Save {
            paths,
            force,
            allow_secrets,
        }) => {
            let mut ctx = Ctx::load(&global)?;
            save::run(&mut ctx, &paths, force, allow_secrets)
        }
        Some(Command::Restore { paths, force }) => {
            let mut ctx = Ctx::load(&global)?;
            restore::run(&mut ctx, &paths, force)
        }
        Some(Command::Sync {
            paths,
            allow_secrets,
        }) => {
            let mut ctx = Ctx::load(&global)?;
            sync::run(&mut ctx, &paths, allow_secrets)
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
        Some(Command::Undo { id }) => {
            let mut ctx = Ctx::load(&global)?;
            undo::run(&mut ctx, id.as_deref())
        }
        Some(Command::Backups { path }) => {
            let ctx = Ctx::load(&global)?;
            backups::run(&ctx, path.as_deref())
        }
        Some(Command::Init { target, dir, force }) => {
            init::run(&global, target.as_deref(), dir.as_deref(), force)
        }
        Some(Command::Git { args }) => {
            let ctx = Ctx::load(&global)?;
            ctx.require_store()?;
            crate::git::passthrough(&ctx.cfg.layout.store, &args)
        }
        Some(Command::Completion { shell }) => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "cubby", &mut std::io::stdout());
            Ok(0)
        }
    }
}
