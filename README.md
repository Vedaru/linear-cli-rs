# linear-cli — Linear from the command line, built for AI agents

A Rust implementation of the unofficial [schpet/linear-cli](https://github.com/schpet/linear-cli),
written for **headless, non-interactive use by agents**. One binary, no runtime, no daemon:
every invocation is a one-shot process that either prints data and exits 0, or prints a
one-line explanation to stderr and exits non-zero.

It keeps upstream's command tree, output strings, error contexts and `--json` shapes, so
prompts, scripts and agent instructions written against upstream keep working.

```
linear issue mine --json              # what am I working on
linear issue view ENG-123             # human-readable detail
linear issue create --team ENG --title "..."   # non-interactive creation
linear api 'query { viewer { name } }'          # raw GraphQL escape hatch
```

## Why it is shaped for agents

| Property | What it means for an agent |
|---|---|
| stdout is data, stderr is diagnostics | warnings never contaminate `--json` output; you can pipe stdout straight into a parser |
| truthful exit codes | `0` success (including "no results"), non-zero for validation, auth, not-found and API failures; never a panic, never a silent empty success |
| no blocking prompts | commands that would confirm interactively require an explicit `--force` / `--yes`; without a terminal they fail fast with a message instead of waiting on stdin |
| `--json` preserves GraphQL field names | payloads are `camelCase` exactly as Linear's API docs describe them, so they concatenate cleanly into agent context |
| one-shot process, no state | nothing is resident between calls; commands are safe to run concurrently |
| no runtime to install | a single ~4.8 MiB binary linking only libc/libgcc — no Node/Deno/Python, so it drops into a minimal container |
| small memory footprint | ~5 MiB peak for typical commands (measured with the kernel's VmHWM); the heaviest command, `schema`, peaks ~15 MiB |
| pipeline-safe | a closed reader (`linear issue list | head`) ends the process the conventional way — no panic, no panic message on stderr |

## Install

From a release tarball (the archive extracts into a top-level directory containing
`linear`, `README.md` and `AGENTS.md`):

```sh
tar xf linear-cli-rs-<sha>-x86_64-unknown-linux-gnu.tar.gz
install -m 0755 linear-cli-rs-<sha>-x86_64-unknown-linux-gnu/linear ~/.local/bin/linear
```

Or build it:

```sh
cargo build --release        # target/release/linear
cargo install --path .       # onto PATH
```

## Authenticate

Resolution order (highest first): command-line flags, `LINEAR_API_KEY` in the environment,
a project config (`linear.toml`, `.linear.toml` or `.config/linear.toml` in the working
directory or git root), then the global config at `$XDG_CONFIG_HOME/linear/linear.toml`.

For a server or an agent host, prefer the environment or a `0600` global config — **never
pass a key as an argument**, where it lands in shell history and `ps`:

```sh
read -rsp 'Linear API key: ' K && printf 'api_key = "%s"\n' "$K" > ~/.config/linear/linear.toml \
  && chmod 600 ~/.config/linear/linear.toml && unset K
```

`linear auth login` stores a credential in the OS keyring and needs a real terminal; on a
host without a keyring pass `--plaintext` to keep it in `credentials.toml` instead. It
validates the key against Linear before storing anything, and its error output never echoes
the key.

Pin the workspace's team once and no command needs `--team`:

```toml
# ~/.config/linear/linear.toml
api_key = "lin_api_..."
team_id = "35feb448-7bc2-4bcb-a949-a58c7572949a"   # a UUID, a key or a name all resolve
```

## Agent contract

- **Exit codes**: `0` success, `1` for everything the CLI reports (validation, auth,
  not-found, GraphQL errors, HTTP failures). Failures print one line prefixed `✗` plus an
  indented suggestion on stderr, e.g.
  `✗ Failed to fetch projects: Team not found: NOSUCHTEAM` / `  Available teams: WAV (WAVE-cloud)`.
- **`--json` is not universal**: it exists where a machine-readable payload makes sense
  (`issue query`, `issue view`, `project list`, `team list`, `label list`, `user list`,
  `cycle list`, `milestone list`, `initiative list`, `document list`, `template list`,
  the status-update lists, `auth list`, `schema`) but **not on every subcommand**:
  `issue list`, `issue mine` and `issue relation list` have no `--json` at all, so use
  `issue query --json` for machine-readable issue work. Always check
  `linear <group> <sub> --help` before scripting a flag.
- **Empty results**: with `--json` you always get JSON (an empty `nodes` array); on human
  output a listing prints a notice such as `No issues found.` instead. Parse the JSON
  rather than scraping text.
- **Confirmations**: destructive commands (`issue delete`, `issue archive`, `project delete`,
  `document delete`, `initiative delete`, `milestone delete`, `label delete`, `team delete`)
  take `--force`/`--yes`; in a headless run the flag is mandatory.
- **Inputs from files or stdin**: bodies accept `--body-file <path>`, and where a flag is
  omitted the CLI reads stdin rather than opening an editor when there is no terminal.
- **`linear api '<graphql>'`** is the escape hatch for anything the typed commands do not
  cover; pass variables with `--variables <json>` (or stdin).
- **Pipelines**: a reader that closes early is not an error — the process dies by `SIGPIPE`
  with no message on stderr.
- **Environment**: `LINEAR_API_KEY`, `LINEAR_TEAM_ID`, `LINEAR_WORKSPACE`,
  `LINEAR_GRAPHQL_ENDPOINT` (point it at a mock to test), `LINEAR_IGNORE_ENV_FILE=1`,
  `LINEAR_DEBUG=1` (full error context), `NO_COLOR=1`, plus `.env` loading from the working
  directory.
- **External helpers**: `git`, `jj`, `gh`, `$EDITOR` and the pager are invoked with
  deadlines and without inheriting a terminal, so a missing tool or a hung helper cannot
  stall an agent (`issue pull-request` needs `gh`; `team autolinks` needs `gh` and a GitHub
  remote).

## Command reference

Eighteen groups, ninety subcommands. Run `linear <group> --help` for flags — the help text
is the authoritative reference, and it mirrors upstream verbatim.

| Group | Subcommands |
|---|---|
| `auth` | `login` add a workspace credential · `logout` · `list` configured workspaces · `default` · `token` print the configured token · `whoami` · `migrate` plaintext credentials to the keyring |
| `issue` | `id` · `mine` · `query` structured filters · `title` · `start` · `view` · `url` · `describe` · `commits` (jj only) · `pull-request` (gh) · `archive` · `delete` · `create` · `update` · `comment` (add/list/…) · `attach` sidebar link · `link` a URL · `relation` dependencies · `agent-session` |
| `project` | `list` · `view` · `create` · `update` · `delete` · `comment` |
| `project-update` | `create` · `list` project status updates |
| `team` | `create` · `delete` · `list` · `id` · `autolinks` (gh) · `members` · `states` workflow states |
| `user` | `list` workspace members |
| `cycle` | `list` · `view` |
| `milestone` | `list` · `view` · `create` · `update` · `delete` |
| `initiative` | `list` · `view` · `create` · `update` · `archive` · `unarchive` · `delete` · `add-project` · `remove-project` · `comment` |
| `initiative-update` | `create` · `list` initiative timeline posts |
| `label` | `list` · `create` · `delete` |
| `template` | `list` · `view` what a template pre-fills |
| `document` | `list` · `view` · `create` · `update` · `delete` · `comment` |
| `config` | interactively generate `.linear.toml` (skip in automation — write the file instead) |
| `schema` | print the GraphQL schema, or `--json` for the raw introspection result |
| `api` | raw GraphQL request |
| `markdown` | the Linear-flavoured markdown reference (mentions, collapsible sections) |
| `completions` | `bash` · `zsh` · `fish` · `powershell` |

## Patterns worth copying

```sh
# Discover before writing: ids, teams and states
linear team list --json | jq -r '.nodes[] | "\(.key)\t\(.id)"'
linear team states ENG --json | jq -r '.[] | select(.type=="started") | .id'

# Triage loop, machine-readable end to end
linear issue mine --json
linear issue query --team ENG --state started --json

# Create with a body from a file, then move it along
linear issue create --team ENG --title "Fix login redirect" --body-file /tmp/body.md --json
linear issue update ENG-123 --state "In Progress"
linear issue comment add ENG-123 --body-file /tmp/note.md

# Link context: a URL on the issue, a dependency between issues
linear issue link ENG-123 https://example.com/logs
linear issue relation add ENG-123 blocks ENG-124

# Anything not covered by a typed command
linear api 'mutation { issueUpdate(id: "...", input: { title: "New" }) { success } }'
```

## Write-path safety

The CLI will do exactly what you ask, including deleting. For agent use:

1. Read before writing — list with `--json` and take IDs from the output rather than
   inventing or reusing stale ones.
2. There is no upsert: creating is not idempotent, so search for an existing item first.
3. Destructive calls need `--force`/`--yes`; prefer `archive` over `delete` where the
   workspace's history matters.
4. Never run workspace-level changes (`team delete`, credential changes) without a human
   in the loop.
5. On HTTP 429/5xx the CLI reports the status and exits non-zero; it does not retry —
   add backoff in the caller.

## Known limitations (from a live audit)

Every group and subcommand was exercised against a real workspace; the full pass/fail
matrix, with repros, is in [docs/CLI-AUDIT.md](docs/CLI-AUDIT.md). The gaps worth knowing
before you rely on something:

- **branch-state issue resolution is not implemented**, so `issue id` and the optional
  `[issueId]` arguments need the identifier passed explicitly (passing IDs is the reliable
  pattern for agents anyway);
- **`gh`-dependent commands** (`issue pull-request`, `team autolinks`) need the GitHub CLI
  and a GitHub remote; they fail cleanly without them;
- **cycles** require cycles to be enabled for the team, otherwise `cycle list`/`view`
  report that cleanly;
- some upstream wordings are reproduced verbatim, quirks included — for example the bulk
  failure line reads `Failed to delet all N issues`, because upstream derives its verb by
  stripping a trailing "ed" from `deleted`. Kept for output parity rather than silently
  improved.

## Webhook service

The same binary also runs as a service that accepts webhook deliveries (and, from M3,
mirrors work between the platforms behind them). It reads the same `linear.toml`:

```toml
[bridge]
bind = "127.0.0.1:8787"

[platform.forgejo]
type = "forgejo"
secret_env = "FORGEJO_WEBHOOK_SECRET"

[[mapping]]
source = "linear:VED"
sink = "forgejo:Vedaru/linear-cli-rs"
```

```sh
LINEAR_WEBHOOK_SECRET=... FORGEJO_WEBHOOK_SECRET=... linear webhook serve
linear webhook serve --check      # resolve the config, print it, do not bind

linear sync                       # or work on demand, with no service at all:
linear sync --apply               #   the plan, and then the plan carried out
linear sync status                # what the store holds, and what it gave up on
```

`linear sync` is the same engine driven by a command instead of by a delivery: it reads
both ends of every mapping, works out which side moved from the revision each link
recorded, and writes the difference. It is a **dry run** unless `--apply` is given - a
sweep can create, so the safe default is to look first, and the plan it prints is the
plan that runs.

`linear sync status` answers the other question a quiet setup raises - whether the queue
drained or filled, which look identical from outside because both answer a webhook with `202`
and neither writes anything. It reads the same store the service writes, prints the queue and
the mappings, and lists the deliveries the queue gave up on with the error that stopped each
one. A store it cannot read is a failure, not a zero.

A config for this shape needs no webhook secrets at all - just the credentials it writes
with - because a sweep never verifies a delivery. The same file is still refused by
`webhook serve`, which says which platform would need one.

`POST /webhooks/<platform>` verifies the signature against the raw body, stores the delivery
and answers `202`; the work happens off the request path. `GET /healthz` reports store
liveness and delivery counts.

**Platforms are configuration, not code.** Each platform is described by a spec: where its
signature lives, where its event name lives, and how to address the entity in the payload
(JSON pointers for the id, scope, URL, actor, comment body, reference text, a review
request's merge state, and an optional
fan-out array for a push). The presets in [`crates/bridge/presets/`](crates/bridge/presets)
- `linear`, `forgejo`/`gitea`/`codeberg` - are exactly that description,
so `type = "forgejo"` and an inline `[platform.<name>.spec]` run the same engine, and a
platform nobody has written a preset for costs a configuration change rather than a pull
request. See [crates/bridge/README.md](crates/bridge/README.md).

## Development

```sh
cargo build --release
cargo test --locked                     # 225 tests
cargo clippy --all-targets --locked -- -D warnings
```

The release profile is tuned for distribution, not for speed: `lto`, `codegen-units = 1`,
`strip`, `opt-level = "z"` and `panic = "abort"` cut the binary from 8.25 MiB to 4.74 MiB,
and every command is network-bound so the slower code is invisible next to an API round
trip. A release-mode test run would need `-Z panic-abort-tests`.

The bridge adds 1.64 MiB on top of that (4.74 -> 6.38 MiB): SQLite (bundled, so no system
library is needed to run it), a small sync HTTP server, TOML parsing and HMAC. Memory is
bounded by construction rather than by tuning - bodies are capped while reading, one
delivery is in flight per worker, and the queue lives in the database rather than in
memory - so a burst costs disk, not RSS.

CI gates every push on `build`, `test`, `clippy -D warnings` and a release build, then
publishes a rolling release. Output fidelity is enforced where it matters: `linear schema`
is checked byte-for-byte against goldens captured from the pre-change binary, so a
refactor that alters a single byte of SDL fails the suite.

One caveat when running the suite by hand: the CLI resolves configuration from the
environment, so a real `~/.config/linear/linear.toml` (or an exported `XDG_CONFIG_HOME`)
leaks into the tests and some of them will fail on a machine that is configured. Run
`env -u XDG_CONFIG_HOME HOME=$(mktemp -d) cargo test --locked` to see the true result.

## Provenance and licence

This is an independent port of [schpet/linear-cli](https://github.com/schpet/linear-cli) by
Peter Schilling and contributors, and is not affiliated with Linear. It is distributed under
the same ISC licence — see [LICENSE](LICENSE).
