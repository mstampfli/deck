# Deck Architecture

Deck is a single Rust binary crate. It is organized as small modules around a
few stable primitives:

- `Project`: the discovered view of a local project.
- `CommandSpec`: a runnable command, either shell-backed or direct `argv`.
- `DeckConfig`: the project-local `deck.toml` schema.
- `State`: the user-local persisted runtime state.
- Output contracts in `contracts.rs`: every command output is one struct with a
  JSON form (serde) and a human form (`Render`), printed through `emit`.

The CLI is intentionally thin. Commands parse arguments in `cli.rs`, select
projects through `selection.rs`, then delegate to a feature module.

## Runtime Flow

Typical command execution:

1. `cli.rs` parses top-level commands; the global `--json` flag selects the
   rendering and whether errors use the JSON envelope.
2. `selection.rs` loads projects, state, and state paths.
3. `discover.rs` builds fresh project views from scan roots and state.
4. `adapters.rs` detects commands from `deck.toml`, Cargo, npm, Make, just, and
   tools such as git.
5. `commands.rs` routes run/list/process operations.
6. `process.rs` executes commands, streams output, and writes run logs.
7. `state.rs` records run summaries and process records under XDG state.

Agents follow the same path as humans: there is no separate agent namespace.
The global `--json` flag switches every command to its structured rendering.

## Module Map

| Module | Responsibility |
| --- | --- |
| `main.rs` | Binary entry point and module registration. |
| `cli.rs` | Top-level CLI parsing, the global `--json` flag, dispatch, and JSON-error selection. |
| `adapters.rs` | Tool and config adapters that turn external project files into `CommandSpec` values. |
| `capabilities.rs` | Machine-readable manifest of every command, its argv shape, and output type. |
| `commands.rs` | Shared handlers for project listing, command execution, process listing, and workflows. |
| `config.rs` | `deck.toml` schema, default config generation, atomic writes, and config locking. |
| `config_edit.rs` | `deck config` mutations of project config: commands, workflows, plugins, and sandbox profiles. |
| `context.rs` | Deterministic context bundles for agents and external tools. |
| `contracts.rs` | Output contracts: serializable shapes, their human renderings, and `emit`. |
| `discover.rs` | Filesystem scanning and project detection. |
| `errors.rs` | Maps internal error messages to stable JSON error kinds. |
| `history.rs` | Read-only run history views and `deck rerun` over existing state. |
| `model.rs` | Core domain structs used across the crate. |
| `planner.rs` | Dry-run command and workflow plans. |
| `plugin.rs` | Registered plugin command execution and plugin protocol helpers. |
| `process.rs` | Process spawning, output capture, server lifecycle helpers, and run logs. |
| `safety.rs` | Computed command safety metadata such as shell usage and locked-sandbox compatibility. |
| `sandbox.rs` | Bubblewrap sandbox planning, execution, diagnostics, policy validation, and presets. |
| `selection.rs` | Shared loading/filtering/project-command selection helpers. |
| `state.rs` | XDG state load/save, run history, process records, and global plugin registry. |
| `summary.rs` | Project startup bundle: context, command safety, sandbox profiles, and suggested next commands. |
| `tasks.rs` | Project-local task CRUD backed by `deck.toml`. |
| `tools.rs` | Thin wrappers for existing tools such as git, docker, gh, rg-like search, ssh, and journalctl. |
| `tui.rs` | Terminal UI entry point and navigation. |
| `workflow.rs` | Workflow selection and sequential workflow execution. |

## Data Ownership

Deck has two persistent data sources:

- Project-local `deck.toml`, managed by `config.rs` and `config_edit.rs`.
- User-local XDG state, managed by `state.rs`.

`deck.toml` owns project intent: commands, workflows, plugins, sandbox profiles,
tasks, and paths. It is locked with `.deck.toml.lock` and written through a
temporary file plus atomic rename.

XDG state owns runtime observations: scanned projects, recent runs, process
records, and global plugin registrations. It should not become a second project
configuration format.

## Commands

Commands are represented by `CommandSpec`.

- `command`: display/log form.
- `argv`: optional direct executable form.
- `kind`: one-shot or server.
- `available`: whether the required tool is present.

Prefer direct `argv` for commands that agents or locked sandbox profiles should
run safely. Shell commands remain supported for ergonomics, but `safety.rs` marks
them as requiring a profile with `allow_shell = true`.

## Sandboxing

Sandboxing is explicit. Normal `deck run` does not sandbox.

`sandbox.rs` is split into these primitives:

- profile loading and preset expansion
- policy validation for writable paths, env names, and timeouts
- Bubblewrap argv construction
- execution with optional timeout
- doctor probes and failure diagnosis

The secure direction is:

- `network = false` for real isolation
- `readonly_project = true`
- small project-relative writable paths
- allowlisted env vars
- `allow_shell = false` with direct `argv` commands

`deck sandbox doctor --json` explains whether the current parent environment can
create the needed namespaces. This matters when Deck itself is run inside another
sandbox.

## Agent Surface

Agents use the same commands as humans plus the global `--json` flag; a command
is only allowed under a machine-only surface if a human genuinely cannot use it.
Today that is exactly one command: `deck capabilities`, the manifest agents read
to discover the CLI. The usual bootstrap sequence is:

- `deck capabilities`: discover commands, argv shapes, and output types.
- `deck summary PROJECT --json`: one startup bundle with context, command
  safety, sandbox profiles, tasks, and suggested next commands.
- Any other command with `--json` as needed.

`--json` failures use the shared error envelope from `contracts.rs` and exit
nonzero. A failed run/workflow/sandbox command prints exactly one result
document (`ok: false`) instead of a second error document.

## Adding Features

Use this checklist for new features:

1. Put domain types in `model.rs` only if they are cross-cutting.
2. Keep persistent schemas in `config.rs` or `state.rs`, not ad hoc files.
3. Add output structs to `contracts.rs` (or the owning module) and implement
   `Render` so the command has both JSON and human forms by construction.
4. Route CLI parsing through `cli.rs`, but keep behavior in a focused module.
   Never add a machine-only command unless a human genuinely cannot use it;
   `--json` is the machine rendering of the one shared surface.
5. Use `selection.rs` for loading and selecting projects/commands.
6. Add unit tests for the primitive and integration tests for public CLI behavior.
7. Update `README.md` and this file when the user-facing or module surface changes.

## Verification

Before calling a change complete:

```sh
/home/parrot/.cargo/bin/cargo fmt --all --check
/home/parrot/.cargo/bin/cargo test
/home/parrot/.cargo/bin/cargo clippy --all-targets -- -D warnings
/home/parrot/.cargo/bin/cargo doc --no-deps
```

