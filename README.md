# cubby

Keep copies of your dotfiles in a **store**: a directory that mirrors your home
directory, with every tracked file at the same relative path. Version the
store with git and your configuration follows you to every machine.

```
~/.zshrc                 →  ~/.dotfiles/.zshrc
~/.config/nvim/init.lua  →  ~/.dotfiles/.config/nvim/init.lua
```

cubby only ever **copies** files. It never symlinks into the store, never
follows symlinks, and never deletes anything from your home directory (the
one exception: `cubby undo` takes back files that the run it reverses
created). Every file it overwrites or removes is backed up first, and
`cubby undo` puts it back.

## Install

```sh
brew install yungibly/tap/cubby
```

Or build from source with a Rust toolchain (1.88 or newer):

```sh
cargo install --git https://github.com/yungibly/cubby
```

Prebuilt binaries for macOS and Linux are attached to each
[release](https://github.com/yungibly/cubby/releases).

## Quick start

```sh
cubby init                       # writes ~/.config/cubby/config.toml, creates ~/.dotfiles
cubby ~/.zshrc ~/.config/nvim    # start tracking a file and a whole directory
cubby status                     # what differs between home and the store
cubby                            # save everything that changed at home
cubby git init                   # version the store (cubby git runs git in it)
```

Commit and push the store like any repository (`cubby git add -A`,
`cubby git commit`, `cubby git push`). On a new machine:

```sh
cubby init git@github.com:you/dotfiles.git   # clone the store, preview a restore
cubby restore                                # copy it into your home directory
```

## Commands

| Command | What it does |
| --- | --- |
| `cubby [PATH...]` | Same as `cubby save`. |
| `cubby save [PATH...]` | Copy home → store: what changed at home, and anything new under tracked directories. A directory named here becomes tracked as a whole. Alias: `add`. |
| `cubby restore [PATH...]` | Copy store → home: what changed in the store, and what is missing at home. Never deletes. |
| `cubby sync [PATH...]` | Both at once: each path is copied the way it changed since the last sync. |
| `cubby status [PATH...]` | What differs, and which side changed. `-q` prints nothing and exits 0 when up to date, 1 when anything differs, 2 on error. |
| `cubby diff [PATH...]` | Unified diffs of what `cubby` would save; `-R` shows what `cubby restore` would change instead. Paged when on a terminal. |
| `cubby list` | The store as a tree. `--plain` prints one path per line. Alias: `ls`. |
| `cubby untrack PATH...` | Remove files or a tracked directory from the store, for every machine. Home is untouched. Alias: `rm`. |
| `cubby ignore [PATTERN...]` | Ignore files on every machine. `--here` skips them on this machine only, `--remove` takes a pattern out, and with no patterns it lists them. |
| `cubby history` | What cubby has done, one line per run; `-v` lists the files. |
| `cubby undo [RUN]` | Reverse the last run, or the run named (see `cubby history`). |
| `cubby backups [PATH]` | Where the copies of overwritten and removed files are, or every kept copy of one file. |
| `cubby init [DIR]` | Create the config file and the store. `cubby init URL [DIR]` clones a store instead. |
| `cubby doctor` | Look for problems that are not differences: files git ignores, secrets, loose permissions, large files. |
| `cubby git ARGS` | Run git in the store. |

Flags that work everywhere:

| Flag | Meaning |
| --- | --- |
| `-n`, `--dry-run` | Show what would happen without changing anything. |
| `-y`, `--yes` | Skip confirmation prompts. Required when stdin is not a terminal. |
| `-v`, `--verbose` | List every file, including unchanged ones and previews longer than 40 lines. |
| `--store DIR` | Use another store for this run. |
| `--no-backup` | Skip the backup of overwritten and removed files. |
| `--color WHEN` | `auto`, `always`, or `never`. `NO_COLOR` is honoured. |

`save` and `restore` also take `--force`, and `save` and `sync` take
`--allow-secrets`; both are explained below.

Paths can be absolute, relative to the current directory, or written with a
leading `~/`. They must be inside your home directory and outside the store.

## How tracking works

**Files.** `cubby ~/.zshrc` copies the file into the store. From then on the
file is tracked: `cubby status` compares the two copies, `cubby` saves the home
copy over the store copy when it changes, and `cubby restore` does the
opposite. The store's contents are the list of tracked files, so `git rm` a
file from the store and it is no longer tracked.

**Directories.** `cubby ~/.config/nvim` copies every file under it and records
the directory in the store's manifest. A tracked directory is mirrored as a
whole: files you add at home are saved next time you run `cubby`, and files
you delete at home are removed from the store (after a backup). A directory
that does not exist at home, or exists but holds no files at all, is left
alone in the store with a warning, so running `cubby` with an unmounted volume
cannot wipe anything. Use `cubby untrack` when you really mean to drop a
directory, and `cubby ignore` to drop one file inside it.

**Which side changed.** For each store, cubby remembers on each machine what
every path held the last time home and the store matched. That is how it tells
"changed at home" from "changed in the store" from "changed on both sides", and
"deleted at home" from "new in the store":

- `save` copies what changed at home. A change that arrived in the store (from
  another machine, through `git pull`) is left alone rather than overwritten.
- `restore` copies what changed in the store, and leaves edits you made at home
  alone.
- A file another machine added to a tracked directory is new in the store, and
  is never mistaken for one you deleted at home.
- A path changed on both sides is a conflict: `cubby diff PATH` shows both, and
  `cubby save --force PATH` or `cubby restore --force PATH` picks one.

Where there is no record of the last sync (a new machine, say), `save` and
`restore` copy what differs, as you would expect.

**Paths are tracked as typed.** `cubby ~/.myapp/sub` tracks `.myapp/sub` even
when `~/.myapp` is a symlink to somewhere else, so the store mirrors the paths
you use rather than where the bytes happen to live. Names are stored in
Unicode NFC form, so a file named in decomposed form at home and composed
form in the store is one file, not two. Names that are not valid UTF-8 are
skipped with a note.

**Symlinks** are tracked as symlinks and restored as symlinks, pointing at the
same target. cubby never follows them, so naming a symlink to a directory saves
the link, not the files in it; cubby says so, and suggests the real directory.

**Conflicts.** When home has a symlink where the store has a file (or the
other way round), or a directory where the other side has a file, the path is
reported and skipped. Pass `--force` to replace a file with a symlink or vice
versa; directories are never replaced.

**Permissions.** Git keeps only a file's executable bit, so a store cloned onto
a new machine would restore `~/.netrc` readable by everyone. cubby records the
permissions of files and directories that group and others cannot read in the
manifest when you save, keeps their store copies private too, and applies the
records on restore: files and directories it creates get them, and existing
ones that are looser are tightened. Otherwise a new copy gets the source's
mode, and a replaced file keeps its own permission bits while the executable
bit follows the source. Records only ever get stricter on their own, so a
machine whose files were loosened cannot erase another machine's record by
saving; `cubby save --force PATH` takes home's permissions as they are.

**Ignore patterns** live in the manifest, and `cubby ignore PATTERN` adds one
(offering to remove files from the store that it now hides). A pattern without
a slash matches a file or directory name at any depth (`*.swp`,
`lazy-lock.json`); one with a slash matches a path relative to home
(`~/.config/nvim/secret/**`). `.git` directories, `.DS_Store`, the store's own
`README*`, `LICENSE*`, and manifest, and cubby's own config and state are always
ignored.

**Skipping on one machine.** `cubby ignore --here ~/.config/aerospace` adds a
pattern to this machine's config instead: the path stays in the store for other
machines, but this one leaves it alone, so a Mac-only file does not show as
missing on Linux forever.

**Secrets.** The first time a file goes into the store, cubby checks whether it
looks secret: private permissions, a private key, or a token in a well-known
format (GitHub, GitLab, Slack, AWS, Stripe, Anthropic, OpenAI, Google, npm,
PyPI). Such files are marked in the preview and need a second yes, which
`--yes` does not give; `--allow-secrets` does. New stores also ignore the usual
names of SSH private keys.

## Several machines

```sh
cubby git pull      # bring in other machines' changes
cubby sync          # save what changed here, restore what changed there
cubby git add -A && cubby git commit -m dots && cubby git push
```

`cubby status` ends with the store's git state (changes to commit, commits to
push or pull), and after saving, cubby names any file git would never commit
because an ignore rule matches it. A global `~/.gitignore` saved into the store
becomes the store's own ignore file, which is a common way to lose a dotfile.

### A marker in your prompt

`cubby status --quiet` is cheap enough to run before every prompt: files that
have not changed since it last looked are not read again. In zsh:

```zsh
autoload -Uz add-zsh-hook
cubby_marker() { cubby status --quiet 2>/dev/null; psvar[1]=$([[ $? == 1 ]] && echo '✱') }
add-zsh-hook precmd cubby_marker
PROMPT='%1v%~ %# '
```

The marker appears when anything needs saving or restoring, and stays silent
when cubby is not set up on that machine (exit code 2).

## Safety

- Copies are atomic: written to a temporary file next to the destination and
  renamed into place, so an interrupted run never leaves a half-written file.
- A file is never copied onto itself, and a symlink that points into the store
  (as `stow` would create) is refused outright.
- `restore` never deletes. `save` only removes store files you deleted at home
  under a tracked directory that exists at home. Neither overwrites a change
  made on the other side since the last sync unless you pass `--force`.
- Every run is recorded with copies of what it overwrote or removed, in
  `~/.local/state/cubby/runs/<id>/`. `cubby undo` reverses a run, touching only
  paths that are still as the run left them. Runs are kept for 30 days, and the
  newest five of each kind (save, restore, and so on) are kept however old.
- Bulk operations show a preview and ask before proceeding. Without a
  terminal they refuse to guess and ask for `--yes`.
- Only one cubby changes files at a time; `status --quiet` never waits for it.
- Special files (sockets, devices) are reported and skipped. A file that cannot
  be read, or a conflict, is reported, and `save` and `restore` exit 1.

## Configuration

`~/.config/cubby/config.toml` (or `$XDG_CONFIG_HOME/cubby/config.toml`) is
machine-local:

```toml
store = "~/.dotfiles"   # the directory that mirrors home
backups = true          # keep copies of overwritten and removed files
backup_days = 30        # for this long
skip = [                # in the store, but left alone on this machine
  "~/.config/aerospace",
]
```

The store can also be set per run with `--store DIR` or `CUBBY_STORE`.

The store's manifest, `.cubby.toml`, travels with the store:

```toml
version = 2

dirs = [
  "~/.config/nvim",
  "~/.config/fish",
]

ignore = [
  "*.swp",
  # the plugin manager rewrites this all the time
  "lazy-lock.json",
  "~/.config/fish/fish_variables",
]

[modes]
"~/.netrc" = "600"
"~/.ssh" = "700"
```

cubby edits this file in place when tracked directories, patterns, or
permissions change, and keeps your comments.

Environment variables: `CUBBY_STORE` (store directory), `CUBBY_PAGER` or
`PAGER` (for `diff`), `NO_COLOR`, and `CUBBY_HOME`, which points cubby at
another directory as if it were home (config, state, and the default store all
move under it), useful for trying things out in a sandbox.

State lives in `~/.local/state/cubby/` (or `$XDG_STATE_HOME/cubby/`): `runs/`
(what each run did, and its backups), `index/` (what each store held at the
last sync, and cached file metadata), and `lock`.

## Upgrading from cubby 2

- Once cubby 3 writes a store's manifest (it adds `version = 2`, and `[modes]`
  when you save private files), cubby 2 can no longer read it. Upgrade every
  machine that shares the store.
- On each machine, the first run records what was in sync. Paths that cubby 2's
  `history.log` shows as saved or restored count as synced, so deleting one at
  home still removes it from the store.
- `save` no longer overwrites changes that arrived in the store, and `restore`
  no longer overwrites edits made at home; `--force` does. `save` and `restore`
  exit 1 when a path was skipped for a conflict or an error.
- Backups now live in `runs/`, and `cubby history` lists runs. cubby 2's
  `history.log` and `backups/` stay where they were, are still read, and old
  backups are pruned by the same rules.
- Files that cubby 2 restored on a new machine may be more open than they
  should be. After a save on the machine where they are private, `cubby
  restore` tightens them everywhere else; `cubby doctor` lists them.

## Development

```sh
cargo test                       # unit and end-to-end tests, sandboxed under target/
cargo build --release            # target/release/cubby
CUBBY_HOME=$PWD/sandbox/home cargo run -- status   # play in a fake home
```

Some tests run git, with its configuration isolated from yours.

Releases are built by GitHub Actions when a `v*` tag is pushed. The workflow
builds macOS and Linux binaries, publishes a release, and updates the Homebrew
formula in `yungibly/homebrew-tap`.

## License

MIT
