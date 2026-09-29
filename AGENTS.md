# Agent guide

A Rust port of the [linear-cli](https://github.com/schpet/linear-cli) TypeScript
CLI, built for headless agent use. Upstream TypeScript is the behavioural
reference; this port keeps its command tree, output strings, error contexts and
`--json` shapes so agent prompts written against upstream keep working.

## Build and test

```sh
cargo build                 # debug binary at target/debug/linear
cargo test                  # unit tests + tests/ integration suites
cargo clippy --all-targets  # lint
```

The crate is a single binary (`src/main.rs`); shared code lives in modules under
`src/`. There is no library target, so integration tests exercise the compiled
binary end to end.

## Layout

| Path | Purpose |
| --- | --- |
| `src/cli.rs` | clap command tree, mirroring upstream `src/cli.ts` (aliases, `--workspace`). |
| `src/main.rs` | Entry point: parse, dispatch, print the wrapped error. |
| `src/commands/mod.rs` | Top-level dispatch to one `run()` per command group. |
| `src/commands/<group>/` | One module per upstream `src/commands/<group>` directory; `mod.rs` owns the group's `Args`, `Command` enum and nested dispatch. |
| `src/graphql.rs` | Client: endpoint selection, credentials, pagination, uploads. |
| `src/linear/` | Reusable GraphQL documents and resolvers (teams, issues, projects, users, …). |
| `src/config.rs` | `.linear.toml` + `.env` loading and precedence rules. |
| `src/credentials.rs`, `src/keyring/` | Workspace credentials (file or system keyring). |
| `src/errors.rs` | `CliError` with user-facing message, suggestion and exit code. |
| `src/output.rs`, `src/display.rs`, `src/colors.rs` | Stdout helpers, tables/relative time, ANSI styling (`NO_COLOR` aware). |
| `src/markdown.rs`, `src/prosemirror.rs` | Linear markdown reference and ProseMirror → markdown conversion. |
| `src/vcs.rs`, `src/git.rs`, `src/jj.rs` | Branch/issue-id detection across git and jj worktrees. |
| `src/editor.rs`, `src/pager.rs`, `src/proc.rs`, `src/upload.rs` | `$EDITOR`, paging, subprocess runners, signed-URL uploads. |
| `tests/common/mod.rs` | Headless mock Linear GraphQL server and `run_cli` helper. |

## Conventions

- **One command group per directory.** `mod.rs` defines `<Group>Args`,
  `<Group>Command` and `run()`; each subcommand lives in its own file and
  exports both its `Args` struct and `run()`.
- **Error context lives in the group.** Wrap a subcommand's error with the same
  prefix upstream surfaces (e.g. `Failed to update milestone`) in `mod.rs`, not
  in the subcommand.
- **Interactive prompts are gated.** Every prompt checks
  `prompt::is_interactive()`; in non-interactive runs, emit upstream's
  validation error naming the flag the caller should pass instead.
- **`--json` preserves key order.** `serde_json` uses the `preserve_order`
  feature so output matches upstream's object ordering; emit raw GraphQL shapes
  rather than re-serializing through typed structs where order matters.
- **No colour when piped.** Use `src/colors.rs`; it honours `NO_COLOR` and TTY
  detection so snapshots stay stable.

## Testing

`tests/common/mod.rs` starts a dependency-free mock Linear API on an ephemeral
port and exposes:

- `MockLinearServer::start(responses)` with `MockResponse::new(query_name, body)`
  and optional `.with_variables(..)` / `.with_query_includes(..)` matchers.
- `run_cli(args, env)` runs `target/debug/linear` with `NO_COLOR=1` and, via
  `mock_env(&server)`, `LINEAR_API_KEY` + `LINEAR_GRAPHQL_ENDPOINT` pointed at
  the mock.
- `MockLinearServer::uploads()` returns captured signed-URL `PUT` bodies.

See `tests/mock_server.rs` (harness self-tests) and `tests/cli_smoke.rs`
(end-to-end command tests) for the pattern.

## Known deviations from upstream

- There is no terminal markdown renderer equivalent to `@littletof/charmd`;
  descriptions and template bodies are emitted as raw markdown (the non-TTY
  path upstream already uses).
- `completions` emits `clap_complete` scripts, whose text differs from cliffy's
  built-in generator.
- `api`'s schema printer reimplements SDL from introspection JSON; it matches
  `graphql-js` on the fixtures exercised but has not been checked against every
  exotic introspection feature.
- `label delete <name>` surfaces a failed lookup request as that error instead of
  reporting the label missing: upstream's `catch` turns any failure — including
  Linear's own rejection of the operation — into "Label not found", which hides
  the cause.
- `initiative update --status` accepts any casing and sends the canonical enum
  value (`Active`, not `active`); upstream lower-cases it, so every status update
  it sends is rejected by the API.
