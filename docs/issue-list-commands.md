# Issue list commands — port reference (`issue mine`, `issue query`)

Distilled from upstream `src/commands/issue/issue-mine.ts` and
`issue-query.ts` (flattened at `/tmp/up/issue/`). The Rust ports live in
`src/commands/issue/issue_mine.rs` and `issue_query.rs`; the table both use is
`src/issue_table.rs`. Read this instead of the TypeScript.

Both commands print the same table (see [`src/issue_table.rs`](#shared-renderer)).
Differences are flagged per command.

## Shared conventions

- No spinner in the port (upstream gates on `shouldShowSpinner()`).
- `resolveIssueSort(flag)` → `config::resolve_issue_sort(cli_value: Option<&str>) -> Result<IssueSort>`.
- Team key: `linear::get_team_key() -> Option<String>` (upper-cased);
  with provenance `linear::get_team_key_with_source() -> Option<config::Resolved<String>>`.
- `getTeamKey()` in upstream `openTeamAssigneeView` is the raw configured key
  (`linear::get_team_key()`), same as elsewhere.
- `isLinearUuid` → `linear::is_linear_uuid`.
- State scope: `StateScope::TeamKeys(vec)` / `StateScope::AllTeams`;
  `linear::resolve_state_selection(&[String], &StateScope) -> Result<StateSelection>`.
- Project resolution (identical in both commands):
  1. `linear::get_project_id_by_name(project) -> Result<Option<String>>`; if `Some`, done.
  2. else `linear::get_project_options_by_name(project) -> Result<Vec<(id, name)>>`;
     empty → `CliError::not_found("Project", project)`.
  3. non-interactive (`!prompt::is_interactive()`) →
     `CliError::validation(format!("Project \"{project}\" not found. Similar projects: {names joined \", \"}"))`.
  4. else `linear::select_option("Project", project, &options) -> Result<Option<String>>`.
- Cycle: `linear::get_cycle_id_by_name_or_number(team_id, cycle)` — **team id first**.
- Milestone: `linear::resolve_milestone_id(name_or_id, project_id: Option<&str>) -> Result<String>`.
- Pagination/limit: `limit == 0` means unlimited; `mine` maps it to `None`,
  `query` passes `Some(0)` (the resolver treats `0` as fetch-all). `query`
  rejects `limit < 0` (parse the flag as `i64`, not `u32`).

## `issue mine` (`issue_mine.rs`)

Upstream `.name("mine")`, aliases `list`/`l` (set in `issue/mod.rs`).

### Flags
`-s/--state` (collect, default `["unstarted"]`), `--all-states`,
`--sort <manual|priority>`, `--team`, `--project`, `--project-label`,
`--cycle`, `--milestone`, `-l/--label` (collect), `--limit` (default 50),
`--created-after`, `--updated-after`, hidden `--assignee` / `-A/--all-assignees`
/ `-U/--unassigned` (removed), `-w/--web`, `-a/--app`, `--no-pager`.

Hidden removed flags must still parse: `#[arg(long = "assignee", hide = true)]`,
`#[arg(short = 'A', long = "all-assignees", hide = true)]`,
`#[arg(short = 'U', long = "unassigned", hide = true)]`.

### Flow
1. `use_pager = pager` (`--no-pager` → `ArgAction::SetFalse`, default true).
2. If `web || app` → `actions::open_team_assignee_view(app)` and return
   (upstream does this *outside* the `try`; so does the port — no
   `"Failed to list issues"` context on this branch).
3. Removed-flag error: if `assignee.is_some() || all_assignees || unassigned`,
   pick the flag name in that order and
   `CliError::validation(format!("{flag} has been removed from 'issue mine'"))
     .suggestion(format!("Use 'linear issue query {flag}' for assignee filtering."))`.
4. `state_array = state` collected (default `["unstarted"]`).
5. `all_states && (state_array.len() > 1 || state_array[0] != "unstarted")` →
   `"Cannot use --all-states with --state flag"`.
6. `sort = resolve_issue_sort(sort_flag)`.
7. `explicit_team = team.map(linear::resolve_team)`; `team_key = explicit_team.key.or(get_team_key())`.
   None →
   `CliError::validation("No default team configured and no team scope provided").suggestion(if git::is_inside_git_repo() { "Use --team <key, name, or ID> to specify a team, or run `linear config` to link this repository to a team." } else { "Use --team <key, name, or ID> to specify a team." })`.
8. `project && project_label` → `"Cannot use --project and --project-label together"`,
   suggestion `"Use --project to filter by a single project, or --project-label to filter by all projects with a given label."`.
9. Resolve `project_id` (shared algorithm).
10. Cycle: `team_id = explicit_team.id.or(resolve_team(team_key).id)`; `cycle_id = get_cycle_id_by_name_or_number(team_id, cycle)`.
11. Milestone: `project_label` present → `"--milestone cannot be used with --project-label"`,
    suggestion `"Use --project to specify a single project when filtering by milestone."`.
    UUID → pass through; else `project_id` required →
    `"--milestone requires --project to be set"`,
    suggestion `"Use --project to specify which project the milestone belongs to, or pass a milestone UUID directly."`,
    then `resolve_milestone_id`.
12. `label_names = labels.is_empty().then(None).unwrap_or(labels)`.
13. `state_selection = all_states ? None : resolve_state_selection(&state_array, &StateScope::TeamKeys(vec![team_key]))`.
14. Fetch:
    `fetch_issues_for_state(&team_key, state_selection.as_ref(), &options)`
    where `options` has `assignee: None, unassigned: false, all_assignees: false,
    limit: (limit == 0).then(|| None).unwrap_or(Some(limit))`, plus
    `project_id`, `sort: Some(sort)`, `cycle_id`, `milestone_id`,
    `project_label`, `label_names`, `created_after`, `updated_after`.
    Read `result["issues"]["nodes"]` (array, default empty).
15. Empty → `output::line("No issues found.")` and return.
16. Render table: `issue_table::render(&issues, &Options { show_team_column: false, show_assignee_column: false, min_title_width: 0, padding: 1 })`.
17. `pager::should_use_pager(lines.len(), use_pager)` → `pager::pipe_to_user_pager(&lines.join("\n"))`, else `output::line` each.
18. Wrap the whole try body: `error.with_context("Failed to list issues")`.

## `issue query` (`issue_query.rs`)

Upstream `.name("query")`, alias `q`.

### Flags
`--search`, `--search-comments`, `--team` (collect), `--all-teams`,
`-s/--state` (collect), `--all-states`, `--assignee`, `-A/--all-assignees`,
`-U/--unassigned`, `--sort`, `--project`, `--project-label`, `--cycle`,
`--milestone`, `-l/--label` (collect), `--limit` (default 50),
`--created-after`, `--updated-after`, `--include-archived`, `-j/--json`,
`--no-pager`.

Four beyond upstream (added 2026-10-03, VED-56/VED-60 - see §"The list levers" below):
`--since <AGE|DATE>` (ages `7d`/`2w`/`3mo`/`36h`, or an absolute date, resolving to the same
`updatedAt` bound as `--updated-after`), `--count-only`, `--group-by <FIELD>` and `--ndjson`.

### Validation (in this exact order)
1. `team_refs.len() > 0 && all_teams` → `"Cannot use both --team and --all-teams flags"`.
2. more than one of `[assignee, all_assignees, unassigned]` →
   `"Cannot specify multiple assignee filters (--assignee, --all-assignees, --unassigned)"`.
3. `all_states && !state_array.is_empty()` → `"Cannot use --all-states with --state flag"`.
4. `project && project_label` → same pair as `mine`.
5. `milestone.is_some() && project.is_none() && !is_linear_uuid(milestone)` →
   `"--milestone requires --project to be set"` (same suggestion as `mine`).
6. `milestone.is_some() && project_label.is_some()` →
   `"--milestone cannot be used with --project-label"`, suggestion
   `"Use --project to specify a single project when filtering by milestone."`.
7. `search_comments && search.is_none()` →
   `"--search-comments requires --search to be set"`, suggestion
   `'Use --search to provide a search term, e.g. --search "oauth timeout" --search-comments.'`.
8. `sort_flag.is_some() && search.is_some()` →
   `"--sort cannot be used with --search"`, suggestion
   `"Search results use relevance ordering. Remove --sort when using --search."`.
9. `limit < 0` → `"--limit must be 0 or greater"`.
10. `since.is_some() && updated_after.is_some()` → `"Cannot use both --since and --updated-after"`
    (the same bound in two notations).
11. `count_only && search.is_some()` → `"Cannot use --count-only with --search"` - Linear's search
    returns no count, and a number nobody can compute is worse than an error.
12. `--group-by <unknown field>` → `'Unknown --group-by field: "<field>"'` - parsed, not accepted
    as a string, because a listing that looks grouped but is not is worse than a refusal.
13. `--ndjson` with `--json`, `--count-only`, `--group-by` or `--search` → four refusals, each
    naming the alternative. A stream cannot honestly produce a count or a group, cannot be merged
    with a document, and search answers in one relevance-ordered page.

### Team scope
- `all_teams` → `team_keys = None`, `is_multi_team = true`.
- `team_refs` non-empty → `resolve_teams`; `team_keys = keys`; `is_multi_team = len > 1`;
  `explicit_team_id = (len == 1).then(teams[0].id)`.
- else `get_team_key_with_source()`; None →
  `CliError::validation("No default team configured and no team scope provided").suggestion("Use --team <key, name, or ID> to specify a team, or --all-teams to query the whole workspace.")`.
  If `should_show_default_team_note(source)` →
  `eprintln!("Note: using default team {key}. Pass --team <key, name, or ID> or --all-teams to be explicit.")`.
  `team_keys = Some(vec![key])`.

`shouldShowDefaultTeamNote(source)`: `false` for `Cli`, `ProjectEnv`,
`ProjectConfig`; `true` for `Env`, `GlobalConfig`. Implemented in
`issue_query.rs`.

### Entity resolution
- `state_selection`: only when `state_array` non-empty; scope is
  `TeamKeys` when `team_keys.is_some()`, else `AllTeams`.
- project: shared algorithm.
- cycle: requires exactly one scoped team. If `!team_keys.map(len==1)` →
  `"--cycle requires a single team scope"`, suggestion
  `"Use --team <key, name, or ID> to specify exactly one team when filtering by cycle."`.
  `team_id = explicit_team_id.unwrap_or(resolve_team(team_keys[0]).id)`.
- milestone: UUID pass-through, else `resolve_milestone_id(milestone, project_id)`.
- `label_names` as in `mine`.

### Fetch & output
`sort = search.is_some() ? None : Some(resolve_issue_sort(sort_flag))`.

- Search mode (`search.is_some()`): trim the term; empty →
  `"--search term cannot be empty"`. Call
  `search_issues_by_term(term, { team_keys, state, assignee, unassigned,
  limit: (limit == 0).then_some(0).unwrap_or(Some(limit)), project_id,
  project_label, cycle_id, label_names, created_after, updated_after,
  include_comments: Some(search_comments), include_archived })`.
- Filter mode: `fetch_issues_for_query({ team_keys, all_teams, state, assignee,
  unassigned, sort, limit: same, project_id, project_label, cycle_id,
  milestone_id, label_names, created_after, updated_after, include_archived })`.

Both then:
- `json` → `output::print_json(&result)` (raw shape, preserves key order); return.
- `result["nodes"]` empty → `output::line("No issues found.")`; return.
- `show_assignee = assignee.is_none() && !unassigned`.
  `issue_table::render(&nodes, &Options { show_team_column: is_multi_team, show_assignee_column: show_assignee, min_title_width: 10, padding: 0 })`.
- Paged print as in `mine`.

Error context: `"Failed to query issues"`.

## The list levers (beyond upstream)

Three questions an agent asks before listing anything, each answered as cheaply as the API allows.

### `--count-only`
`IssueConnection` has no count field - asking for `totalCount` is a validation error - and the only
count in the schema is `Team.issueCount`, which takes no filter arguments. So there are two
mechanisms, and the flag does not pretend otherwise: an **unfiltered** count is that single field
(no nodes, no pages at all), and a **filtered** count asks for `nodes { id }` - the smallest thing
an issue can be - and counts them, one request while the answer fits in one page and a cursor walk
when it does not. `--count-only --search` is refused. With `--json` it prints `{"count": N}`.

### `--since`
`7d` / `2w` / `3mo` / `36h`, or an absolute date, resolved to the same `updatedAt` bound as
`--updated-after` and refused when both are given. The age grammar lives once, in
`src/linear/dates.rs` beside the ISO parser it falls back to - nothing in the CLI parsed an age
before it - and a mistyped age (`7x`) teaches the age grammar rather than the date one.

### `--group-by state|priority|assignee|project`
Grouping happens in the output layer, so `--json` gets a *grouped* document rather than a flat list
an agent would have to re-group:

```json
{ "groupedBy": "state", "total": 3,
  "groups": [{ "label": "In Progress", "count": 2, "issues": [ ... raw nodes ... ] }] }
```

Groups keep first-seen order (the query's own sort decided what matters), the table path prints one
table per group, and an unknown field is a validation error.

### `--ndjson`
One JSON object per line, written the moment each page arrives: the pagination loop lives in
`stream_issues_for_query`, which hands each page to a callback, so a ten-page result starts printing
on page one. The limit is applied *before* a page is handed over - a consumer that writes as it goes
cannot print more than was asked for - and every line is compact and flushed, because stdout is
block-buffered through a pipe and a stream that arrives at exit is a buffered list wearing a
stream's name. Refused with `--json`, `--count-only`, `--group-by` and `--search`.

## Shared renderer

`src/issue_table.rs` ports `formatIssueTable` (query) with the `mine` inline
table folded in. Signature:

```rust
pub struct Options {
    pub show_team_column: bool,
    pub show_assignee_column: bool,
    /// `0` for `mine`, `10` for `query`.
    pub min_title_width: usize,
    /// Columns subtracted before title sizing: `1` for `mine`, `0` for `query`.
    pub padding: usize,
}
pub fn render(issues: &[Value], options: &Options) -> Vec<String>;
```

Terminal width: `stdout().is_terminal()` then `terminal_size::terminal_size()`,
else 120.

Column widths (all `display_width`):
- `PRIORITY=3`, `BLOCKED=1`, `ESTIMATE=1`, `UPDATED=max(7, max time-ago width)`.
- `ID = max(2, max identifier width)`.
- `TEAM = show_team_column ? max(4, max team.key width) : 0`.
- `LABEL = min(25, max(6, max joined-label-name width))`.
- `show_cycle = any(issue.cycle != null || issue.team.cyclesEnabled)`.
  `CYCLE = show_cycle ? max(3, max cycle-short width) : 0`. Cycle short via
  `display::format_cycle_short(cycle_info, team.activeCycle.number)`.
- `ASSIGNEE = show_assignee_column ? 2 : 0`.
- `STATE = min(20, max(5, max state.name width))`.

`fixed_cells = [priority, id, (team), label, blocked, estimate, (cycle),
(assignee), state, updated]`; `fixed = sum + fixed_cells.len() + 1`.
`max_title = max title width`;
`title_width = max(options.min_title_width, min(max_title, columns.saturating_sub(options.padding).saturating_sub(fixed)))`.

Header cells, joined `" "`, whole line `colors::header`:
`◌ ID [TEAM] TITLE LABELS B E [CYC] [A] STATE UPDATED`, each `pad_display`.

Rows joined `" "`:
- priority `pad_display(get_priority_display(issue.priority), 3)`
- identifier `pad_display(id, ID)`
- (team) `pad_display(team.key, TEAM)`
- title `pad_display(truncate_text(title, title_width), title_width)`
- labels: `format_labels` (below)
- blocked: `pad_display(if is_issue_blocked(issue) { colors::warning("⊘") } else { " " }, 1)`
- estimate `pad_display(estimate|"-", 1)`
- (cycle) `color_cycle_short(short) + spaces(CYCLE - width(short.text))`
- (assignee) `pad_display(initials[..2] | "-", 2)`
- state: `color_hex(state.color, truncate_text(name, STATE)) + spaces(STATE - width(truncated))`
- updated: `colors::muted(pad_display(get_time_ago(updatedAt), UPDATED))`

`format_labels` (labels empty → `" ".repeat(LABEL)`): iterate labels with
`separator = if i > 0 { ", " } else { "" }`; if `current + width(separator+name)
> LABEL`, compute `remaining = LABEL - current`; if `remaining >= 4` push
`separator + color_hex(color, truncate_text(name, remaining - separator.len()))`;
break. Else push `color_hex(color, name)` prefixed by separator and add
`width(separator+name)`. Finally pad with spaces to `LABEL` visible width.
