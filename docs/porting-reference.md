# Porting reference (Rust port of linear-cli)

Quick index of the shared APIs and conventions used while porting commands from
the upstream TypeScript at `/tmp/up/`. Read this before grepping `src/` — it
exists so ports don't require re-reading every module.

Signatures below are verified against the tree. If one drifts, fix this file in
the same change.

## Build / test

```sh
cargo build                 # target/debug/linear
cargo test                  # unit + tests/ integration suites
cargo clippy --all-targets
```

## Command layout convention

- One directory per upstream `src/commands/<group>`.
- `<group>/mod.rs` owns `<Group>Args`, `<Group>Command`, `run()`; each
  subcommand file exports its own `Args` struct + `run(args) -> Result<()>`.
- Error context prefix lives in `mod.rs` where upstream puts it, or in the
  subcommand when upstream wraps there (`handleError(err, "Failed to X")`).
- With no subcommand, the group prints help (`augment_args(Command::new("issue"))`).
- Subcommand attributes mirror upstream: `#[command(alias = "v")]`, `#[arg(short = 'w', long)]`.
  clap negation flags: upstream `--no-comments` (default true) is
  `#[arg(long = "no-comments", action = ArgAction::SetFalse, default_value_t = true)] pub comments: bool`
  — or model as `#[arg(long = "no-comments")] no_comments: bool` and invert. Follow
  whatever a sibling command already does.

## Errors — `src/errors.rs`

```rust
pub type Result<T> = std::result::Result<T, CliError>;

CliError::cli(msg)            // generic
CliError::validation(msg)     // bad input
CliError::not_found(type, id) // "Issue not found: ENG-1"
CliError::auth(msg)           // adds "Run `linear auth login` ..."
  .suggestion("...")          // dimmed fix hint
  .with_context("Failed to view issue")  // prefix, as handleError(err, ctx)
  .cause(err) .maybe_suggestion(opt) .with_http_status(u16)
errors::with_context(ctx, || { ... })    // wrap a closure
errors::translate_not_found(type, id, || {...})
errors::handle_not_found(type, id)       // -> impl FnOnce(CliError)->CliError for map_err
errors::handle_error(&err, Some(ctx))    // -> ! prints to stderr, exits 1
errors::is_debug_mode()                  // LINEAR_DEBUG=1
```

Wrap a command body: `let result = (|| -> Result<()> { ... })(); result.map_err(|e| e.with_context("Failed to view issue"))`.

## Output — `src/output.rs`

```rust
output::line(&str)             // writeln stdout, EPIPE-tolerant
output::blank()
output::raw(&str)              // no newline, flushed
output::to_pretty(&Value) -> String   // JSON.stringify(v, null, 2)
output::print_json(&Value)            // line(to_pretty(v))
output::print_json_raw(&str)          // already-formatted JSON
output::warn(&str) / warn_with_suggestion(msg, suggestion)   // stderr
```

## Colors — `src/colors.rs`

`black red green yellow blue magenta cyan white gray bold dim italic underline
strikethrough`; composites `error success info warning muted highlight header`;
`color_hex(hex, text)`; `color_enabled()`, `set_color_enabled(bool)`,
`no_color_env()`, `init()` (stdout TTY + NO_COLOR), `init_stderr()`.
`#[cfg(test)] colors::TEST_LOCK` serializes global-toggle tests.

## Display — `src/display.rs`

```rust
strip_ansi(&str) -> String
strip_console_format(&str) -> String          // removes "%c"
display_width(&str) -> usize
pad_display(&str, width) -> String
pad_display_formatted(&str, width) -> String
truncate_text(&str, max_width) -> String      // unicode-aware, "..."
get_time_ago(DateTime<Utc>) -> String         // coarse, used by lists
format_relative_time(&str) -> String          // RFC3339 in; finer + M/D/YYYY fallback
get_priority_display(i64) -> String           // 0 "---",1 "⚠⚠⚠",2 "▄▆█",3 "▄▆ ",4 "▄  "
get_project_priority_label(i64) -> String
format_cycle_short(Option<CycleDisplayInfo>, Option<i64>) -> CycleShort
color_cycle_short(&CycleShort) -> String
print_members(&[Value], heading)

pub struct CycleDisplayInfo { number: i64, is_active: bool, is_next: bool, is_previous: bool, is_past: bool }
pub enum CycleShortKind { Active, Future, Past, None }
pub struct CycleShort { text: String, kind: CycleShortKind }
```

Build `CycleDisplayInfo` from the GraphQL cycle node; pass the team's
`activeCycle.number` as the anchor.

## Hyperlinks — `src/hyperlink.rs`

```rust
hyperlink(text, url) -> String                 // OSC-8
should_enable_hyperlinks() -> bool             // false if NO_COLOR or !stdout.is_terminal()
should_show_spinner() -> bool
resolve_hyperlink_format(&str) -> String
hostname() -> String
format_path_hyperlink(display_text, path_or_url, format) -> String
```

## Actions — `src/actions.rs`

Port of `src/utils/actions.ts` (shared by `team list` and `issue view`).

```rust
open_issue_page(provided_id: Option<&str>, app: bool) -> Result<()>  // resolves id, prints "Opening …", opens
open_url(url: &str, app: bool) -> Result<()>   // macOS `open [-a Linear]`, Windows `cmd /C start`,
                                               // else `linear` (app) / `xdg-open`
```

## Pager — `src/pager.rs`

```rust
get_pager_command() -> Option<PagerCommand>
pipe_to_user_pager(content: &str)
should_use_pager(output_lines: usize, use_pager: bool) -> bool  // requires use_pager && stdout TTY;
                                                                // height-2, else >50 lines
```

## Markdown — `src/markdown.rs`

```rust
MARKDOWN_HINT: &str
with_markdown_hint(desc) -> String
LINEAR_MARKDOWN_REFERENCE: &str
get_linear_upload_host(url) -> Option<String>
extract_image_info(Option<&str>) -> Vec<ImageInfo>
extract_linear_link_info(Option<&str>) -> Vec<LinkInfo>
image_cache_dir() -> PathBuf                   // $TMPDIR|$TMP|$TEMP else /tmp, + "linear-cli-images"
download_markdown_images(sources: &[Option<&str>]) -> HashMap<String,String>
replace_urls(content: &str, &HashMap<String,String>) -> String   // longer URLs first
sanitize_filename(name: &str) -> String        // pub(crate); no separators/control/leading dots
download_linear_file(url, dest: &Path, label) -> Result<()>      // pub(crate); adds API-key header
                                                                 // for the private upload host
```

`download_linear_file` is the shared auth+fetch+write path; image downloads and
attachment downloads both call it (`label` names the artifact in errors).
`sanitize_filename` is `pub(crate)` for the same reason. Private: `download_image`,
`upload_agent`.

## Config — `src/config.rs` (getters used by commands)

```rust
workspace() -> Option<String>
cli_workspace() -> Option<String>              // from --workspace; prefer first
download_images() -> Option<bool>
hyperlink_format() -> Option<String>
attachment_dir() -> Option<String>
auto_download_attachments() -> Option<bool>
```

Precedence pattern for a workspace: `config::cli_workspace().or_else(config::workspace)`.

## GraphQL client — `src/graphql.rs`

```rust
REQUEST_TIMEOUT: Duration   // 60s
resolve_api_key_opt() -> Result<Option<String>>
resolve_api_key() -> Result<String>
client() -> Result<Client>
client_with_key(&str) -> Result<Client>
```

## Linear resolvers

`src/linear/identifiers.rs`
```rust
get_issue_identifier(provided_id: Option<&str>) -> Result<Option<String>>  // accepts URL or ENG-123
get_issue_id(identifier: &str) -> Result<Option<String>>
```

`src/linear/issues.rs`
```rust
fetch_issue_details_raw(issue_id, include_comments) -> Result<Option<Value>>  // raw GraphQL shape
fetch_issue_details(issue_id, include_comments) -> Result<Value>              // connections -> nodes arrays;
                                                                             // children/attachments/documents always present;
                                                                             // comments only when include_comments
```

`fetch_issue_details` has **no spinner parameter** (upstream's 3rd arg is
display-only and dropped in the port).

## Consts — `src/consts.rs`

```rust
LINEAR_WEB_BASE_URL = "https://linear.app"
LINEAR_PRIVATE_UPLOAD_HOST = "uploads.linear.app"
LINEAR_PUBLIC_UPLOAD_HOST = "public.linear.app"
LINEAR_UPLOAD_HOSTNAMES
```

## Proc — `src/proc.rs`

```rust
DEFAULT_TIMEOUT
proc::run(program, &[args], &RunOptions::default(), DEFAULT_TIMEOUT) -> Option<Output{success,..}>
proc::run_inherit(program, &[args], Option<&[u8]> stdin, timeout) -> Option<bool>
```

## Known porting gotchas

- **No terminal markdown renderer** (`@littletof/charmd` has no Rust equivalent).
  Port the *non-TTY raw-markdown* branch of any view command; use
  `std::io::stdout().is_terminal()` and `terminal_size::terminal_size()` only for
  width/pager decisions. Precedent: `issue/issue_agent_session.rs`,
  `template/template_view.rs` (local minimal `render_markdown`).
- **`open_url` is shared** in `src/actions.rs` (formerly a private copy in
  `team_list.rs`). Use `actions::open_url` / `actions::open_issue_page` rather
  than duplicating the platform opener.
- **`comments::render_comment_threads`** renders the `comment list` bullet format,
  NOT the `issue view` `## Comments` markdown or the underlined terminal header.
  View needs its own helpers mirroring `formatCommentsAsMarkdown` /
  `captureCommentsForTerminal` / `formatCommentHeader`.
- **`--json` preserves key order** (serde_json `preserve_order`); emit raw GraphQL
  shapes, don't re-serialize through typed structs.

## Upstream → Rust file map

| Upstream | Rust |
| --- | --- |
| `src/cli.ts` | `src/cli.rs` |
| `src/commands/<group>/<cmd>.ts` | `src/commands/<group>/<cmd>.rs` |
| `src/utils/errors.ts` | `src/errors.rs` |
| `src/utils/display.ts` | `src/display.rs` |
| `src/utils/pager.ts` | `src/pager.rs` |
| `src/utils/hyperlink.ts` | `src/hyperlink.rs` |
| `src/utils/markdown-images.ts`, `markdown-help.ts` | `src/markdown.rs` |
| `src/utils/graphql.ts` | `src/graphql.rs` |
| `src/utils/linear.ts` | `src/linear/*.rs` |
| `src/utils/actions.ts` | `src/actions.rs` |
