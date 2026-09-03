# cubby

Keep copies of your dotfiles in a **store**: a directory that mirrors your home
directory, with every tracked file at the same relative path. Version the
store with git and your configuration follows you to every machine.

```
~/.zshrc                 →  ~/.dotfiles/.zshrc
~/.config/nvim/init.lua  →  ~/.dotfiles/.config/nvim/init.lua
```

cubby only ever **copies** files. It never symlinks into the store, never
follows symlinks, and never deletes anything from your home directory. Every
file it overwrites or removes is backed up first.

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
cubby restore                    # copy the store back over home (new machine)
```

Then treat the store like any repository:

```sh
git -C ~/.dotfiles init && git -C ~/.dotfiles add -A && git -C ~/.dotfiles commit -m "dotfiles"
```

On a new machine, clone the store to `~/.dotfiles` and run `cubby restore`.

## Commands

| Command | What it does |
| --- | --- |
| `cubby [PATH...]` | Same as `cubby save`. |
| `cubby save [PATH...]` | Copy home → store. With no paths, saves every tracked file that changed. A directory named here becomes tracked as a whole. Alias: `add`. |
| `cubby restore [PATH...]` | Copy store → home. Creates missing files and overwrites modified ones; never deletes. |
| `cubby status [PATH...]` | Show what is modified, new, missing, or conflicting. |
| `cubby diff [PATH...]` | Unified diffs, store → home. `-R` flips the direction. Paged when on a terminal. |
| `cubby list` | The store as a tree. `--plain` prints one path per line. Alias: `ls`. |
| `cubby untrack PATH...` | Remove files or a tracked directory from the store. Home is untouched. Alias: `rm`. |
| `cubby history` | What cubby has done, newest last. |
| `cubby init [DIR]` | Create the config file and the store. |

Flags that work everywhere:

| Flag | Meaning |
| --- | --- |
| `-n`, `--dry-run` | Show what would happen without changing anything. |
| `-y`, `--yes` | Skip confirmation prompts. Required when stdin is not a terminal. |
| `-v`, `--verbose` | List every file, including unchanged ones and previews longer than 40 lines. |
| `--force` | Let `save`/`restore` replace a file with a symlink or vice versa. |
| `--store DIR` | Use another store for this run. |
| `--no-backup` | Skip the backup of overwritten and removed files. |
| `--color WHEN` | `auto`, `always`, or `never`. `NO_COLOR` is honoured. |

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
alone in the store with a warning, so running `cubby` on a fresh machine or
with an unmounted volume cannot wipe anything. Use `cubby untrack` when you
really mean to drop a directory.

**Paths are tracked as typed.** `cubby ~/.myapp/sub` tracks `.myapp/sub` even
when `~/.myapp` is a symlink to somewhere else, so the store mirrors the paths
you use rather than where the bytes happen to live. Names are stored in
Unicode NFC form, so a file named in decomposed form at home and composed
form in the store is one file, not two. Names that are not valid UTF-8 are
skipped with a note.

**Symlinks** are tracked as symlinks and restored as symlinks, pointing at the
same target. cubby never follows them.

**Conflicts.** When home has a symlink where the store has a file (or the
other way round), or a directory where the other side has a file, the path is
reported and skipped. Pass `--force` to replace a file with a symlink or vice
versa; directories are never replaced.

**Permissions.** A new copy gets the source's mode. When a file is replaced it
keeps its own permission bits, but the executable bit follows the source, so
`chmod +x` propagates while a mode 600 file stays 600 after a restore from a
freshly cloned store.

**Ignore patterns** live in the manifest. A pattern without a slash matches a
file or directory name at any depth (`*.swp`, `lazy-lock.json`); one with a
slash matches a path relative to home (`.config/nvim/secret/**`). `.git`
directories, `.DS_Store`, and the store's own `README*`, `LICENSE*`, and
manifest are always ignored.

## Safety

- Copies are atomic: written to a temporary file next to the destination and
  renamed into place, so an interrupted run never leaves a half-written file.
- A file is never copied onto itself, and a symlink that points into the store
  (as `stow` would create) is refused outright.
- `restore` never deletes. `save` only removes store files under a tracked
  directory that exists at home.
- Anything overwritten or removed is copied to
  `~/.local/state/cubby/backups/<timestamp>-<operation>/` first. The newest
  20 backup sets are kept.
- Bulk operations show a preview and ask before proceeding. Without a
  terminal they refuse to guess and ask for `--yes`.
- Special files (sockets, devices) are reported and skipped, and a file that
  cannot be read shows up as an error in `status` rather than as a change.

## Configuration

`~/.config/cubby/config.toml` (or `$XDG_CONFIG_HOME/cubby/config.toml`) is
machine-local:

```toml
store = "~/.dotfiles"   # the directory that mirrors home
backups = true          # keep copies of overwritten and removed files
```

The store can also be set per run with `--store DIR` or `CUBBY_STORE`.

The store's manifest, `.cubby.toml`, travels with the store:

```toml
dirs = [
  "~/.config/nvim",
  "~/.config/fish",
]

ignore = [
  "*.swp",
  "*~",
  "lazy-lock.json",
  "~/.config/fish/fish_variables",
]
```

cubby rewrites this file when tracked directories change, so it keeps only
these two keys; put notes in the store's README.

Environment variables: `CUBBY_STORE` (store directory), `CUBBY_PAGER` or
`PAGER` (for `diff`), `NO_COLOR`, and `CUBBY_HOME`, which points cubby at
another directory as if it were home (config, state, and the default store all
move under it), useful for trying things out in a sandbox.

State lives in `~/.local/state/cubby/` (or `$XDG_STATE_HOME/cubby/`):
`history.log` and `backups/`.

## Development

```sh
cargo test                       # unit and end-to-end tests, sandboxed under target/
cargo build --release            # target/release/cubby
CUBBY_HOME=$PWD/sandbox/home cargo run -- status   # play in a fake home
```

Releases are built by GitHub Actions when a `v*` tag is pushed. The workflow
builds macOS and Linux binaries, publishes a release, and updates the Homebrew
formula in `yungibly/homebrew-tap`.

## License

MIT
