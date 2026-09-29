# linear-cli (Rust port)

A Rust port of the unofficial [linear-cli](https://github.com/schpet/linear-cli),
designed for headless agent use. It keeps upstream's command tree, output
strings, error contexts and `--json` shapes, so prompts and scripts written
against upstream keep working.

## Build

```sh
cargo build --release      # binary at target/release/linear
cargo install --path .     # or install onto PATH
```

## Quick start

```sh
# Non-interactive: provide a key via the environment.
export LINEAR_API_KEY="lin_api_..."        # personal API key
linear issue mine                          # issues assigned to you
linear issue view ENG-123 --json           # machine-readable output
linear api 'query { viewer { id name } }'  # raw GraphQL escape hatch
```

Or store a credential interactively:

```sh
linear auth login        # prompts for the key
linear auth list
linear auth whoami
```

## Configuration

Settings resolve highest-first from: command-line flags, the process
environment (including `LINEAR_*` values loaded from a project `.env`), a
project config file (`linear.toml`, `.linear.toml` or `.config/linear.toml` in
the working directory or git root), then the global config at
`$XDG_CONFIG_HOME/linear/linear.toml`. Run `linear config` to generate one.

| Environment variable | Effect |
| --- | --- |
| `LINEAR_API_KEY` | API key. Cannot be combined with `--workspace`. |
| `LINEAR_TEAM_ID` | Default team (each `.linear.toml` option maps to `LINEAR_<OPTION>`). |
| `LINEAR_DEBUG=1` | Show full error details including stack traces. |
| `LINEAR_IGNORE_ENV_FILE=1` | Skip loading `.env` files. |

`NO_COLOR` disables ANSI styling; output is also unstyled whenever stdout is not
a terminal, which keeps captured output stable.

## Conventions

- Every command that renders structured data accepts `--json`.
- Errors print a message plus a suggested fix and exit non-zero.
- `linear api` runs a raw GraphQL request (with `--paginate`, `@file`
  variables, and `-` for stdin).

See [`AGENTS.md`](AGENTS.md) for the module layout, porting conventions and how
the integration test harness drives the binary against a mock Linear API.
