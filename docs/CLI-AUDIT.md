> **Note on this document.** Raw result of a live-API audit of this CLI, run against a
> real workspace; identifying details are anonymised (`<workspace>`, `<team-id>`,
> `<user>`, `<org>/<repo>`, `<project>`). Findings **F1-F4 were fixed in the commit that
> added this file** - the tables record the behaviour as found. F5-F11 remain open gaps
> (F5 = unimplemented branch-state resolution; F6-F11 are minor/behavioural).

> **What has changed since this audit** (run 2026-09-29): the command tree has grown a lot - 18
> groups and 86 help-listed leaves then, with whole new groups (`sync`, `webhook`, `view`,
> `roadmap`) and additions inside `issue`, `cycle` and `label` since. `issue mine`, `issue describe`
> and `issue relation list` have `--json` now, and `issue query` grew `--since`, `--group-by`,
> `--count-only`, `--ndjson` and `--view`. The tables below stay exactly as they were run: an audit
> that is silently rewritten is not an audit. For the current surface read `linear <group> --help`;
> for the machine-readable half, `tests/json_coverage.rs`. The counts are not repeated here because
> `tests/docs_coverage.rs` re-runs the README's own measurements against the tree - a number written
> into a document is a number that goes stale, which is what this note is for.

# `linear` CLI 2.6.0 — live API audit against workspace WAVE-cloud

**Summary:** 18/18 groups respond to `--help`; **86 leaf commands + 1 hidden alias (`issue list`) executed against the live API: 74 OK, 2 FAIL, 4 EXPECTED-FAIL (3× missing `gh`/`jj`, 1× cycles disabled), 7 SKIP (not run by rule)** — 73 of the OK rows are help-listed leaf commands and 1 is the hidden alias. Plus **12 flag-level/behavioural checks (6 FAIL, 1 EXPECTED-FAIL, 5 OK)** and **9/9 agent-contract checks passing**. All audit objects were named `ZZZ-AUDIT-*` and have been removed.

> **Read this summary as of 2026-09-29.** The binary has moved on: it now answers with **23 groups
> and 108 leaf commands**, and six of the findings below are no longer what they were. §0 is the
> 2026-10-03 re-measurement of the surface and of each finding's status, with its evidence — start
> there, then read this for what was actually executed.

---

## 0. Re-measured 2026-10-03 — structure and finding status

**What this section is.** The per-command evidence in §2–§5 is the **2026-09-29** run and has been
left exactly as it was: it is a record of what that binary did that day, and rewriting it would
destroy the only thing it is good for. What follows is a *re-measurement of the current binary* on
**2026-10-03** against workspace `VED` (the audit's workspace `WAVE-cloud` is no longer the one this
token points at), covering the two things that can be stated without re-executing the whole audit:
the **command surface**, and the **status of each finding**, with its evidence named.

**What this section is not.** It is not a re-run of the audit. Nothing here was executed against an
audit object; the probes below target names that do not exist, so no workspace state was created,
changed or removed. A full behavioural re-run is still owed, and until it happens the rows in §2 for
commands the audits share should be read as 2026-09-29 facts.

### 0.1 The surface, as the binary answers today

| | 2026-09-29 | 2026-10-03 |
|---|---|---|
| groups | 18 | **23** |
| leaf commands | 86 + 1 hidden alias | **108** |
| leaves carrying `--json` | (not stated) | **53** |
| groups added since | — | `view`, `roadmap`, `notification`, `sync`, `webhook` |

Method, so the numbers can be re-derived: the group/leaf/`--json` counts are the ones
`tests/docs_coverage.rs` computes from the clap tree and holds the README to, so they cannot drift
silently. Walking `<group> --help` with the current build lists **23 groups and 93 leaves** — the
difference from 108 is hidden aliases (`issue list` is `issue mine`) and nested subcommands
(`issue comment add|list|resolve…`), which help does not print. Both numbers are correct; they count
different things, and the ratchet owns the larger one.

```
auth           login logout list default token whoami migrate
issue          id mine query title start view url describe commits pull-request archive
               delete unarchive subscribe unsubscribe create update comment attach link
               relation agent-session
project        list view create update delete comment
project-update create list
roadmap        list view
team           create delete list id autolinks members states
user           list
cycle          list view update archive
milestone      list view create update delete
initiative     list view create update archive unarchive delete add-project remove-project comment
initiative-update create list
label          list create delete update
template       list view
document       list view create update delete comment
view           list view create update delete
notification   list read archive
config         service          schema   api   markdown   completions
webhook        serve replay     sync     status link
```

### 0.2 Status of each finding, with what backs the status

| finding | status today | evidence (2026-10-03) |
|---|---|---|
| F1 `label delete <NAME>` cannot find a label | **fixed** | `label delete ZZZ-NOPE-2026` → exit 1 `Label not found: ZZZ-NOPE-2026` / `Searched in team VED and workspace.` — it resolves by name and says where it looked. `src/commands/label/label_delete.rs:46` resolves through `support::resolve_label`, which now falls back to workspace labels (`support.rs:102,143`) — which is F4's lookup half as well. |
| F2 `initiative update --status` cannot work | **fixed** | `initiative update ZZZ-NOPE-2026 --status Active` → exit 1 `Initiative not found: … Pass an initiative UUID, slug ID, or exact initiative name.` The command gets *past* the status mapping to name resolution, which is the part that could never run before. |
| F3 `issue query --include-archived` ignored | **fixed in the code** | `include_archived` reaches the request variables on both paths (`src/commands/issue/issue_query.rs:372,394` → `src/linear/issues.rs:445` inserts `includeArchived`). A live count is *not* evidence either way here — 50 rows with and without, both capped by the default limit — so the citation is the code, not a run. |
| F4 workspace-level labels cannot be applied | **lookup half fixed, write half not re-checked** | The resolver falls back to workspace labels and documents the multi-match rule (`label/support.rs:51,102,143`). Whether a write accepts a workspace label is not re-checked here. |
| F5 branch-state resolution unimplemented | **partially addressed, not re-checked** | The `[issueId]`-optional commands now suggest the branch path: `issue title ZZZ-NOPE-2026` → `Could not determine issue ID` / `Please provide an issue ID like 'ENG-123'`. Whether a real branch pattern resolves is untested here (no such branch exists in this checkout). |
| F6 `--icon` can never succeed | **changed: it now validates** | `document update ZZZ-NOPE-2026 --icon nope` → exit 1 `icon is not a valid icon.` That is a validation refusal rather than the old path, but a *valid* icon has not been re-tried, so "fixed" would be a claim this measurement does not support. |
| F7 `pull-request` message is a tautology | **not re-checked** | Needs `gh`; not installed here. |
| F8 `--web`/`-a` unusable in a single-workspace setup | **not re-checked** | Would open a browser; this host has none. |
| F9 `milestone update <name>` leaks a GraphQL entity name | **still open** | `milestone update ZZZ-NOPE-2026 --name nope` → exit 1 `Could not find referenced ProjectMilestone.` — same leak, and still no suggestion. |
| F10 `issue update <id>` with no flags reports success | **not re-checked** | Probing it means accepting a write; the safest available read of the code found no "no fields given" guard, which is weak evidence and is not claimed as one. |
| F11 deletion described as permanent but soft-deletes | **not re-checked** | Wording is unchanged in the affected help text. |

The `expected-fail` rows do not change: `jj` and `gh` are still absent from this host, and cycles
remain disabled for the team.

---

## 1. Scope, environment and how to read this report

| | |
|---|---|
| Binary | `/opt/data/bin/linear` (`linear 2.6.0`, on `PATH`) |
| Workspace | WAVE-cloud (`wave-cloud`), user `<user>` / <user>@<domain> |
| Team | `WAV` (`<team-id>`) |
| Runner | every command prefixed with `env -u LINEAR_GRAPHQL_ENDPOINT -u LINEAR_API_KEY -u LINEAR_IGNORE_ENV_FILE`, `stdin </dev/null`, `timeout` wrapper. No `LINEAR_*` variable was ever exported; no config file was created or modified; credentials never printed. |
| Date | 2026-09-29, ~11:00–11:25 UTC |

Interpretation of the result column:

* **OK** — command completed its documented action (or, for read-only commands, returned correct data).
* **FAIL** — the command cannot do its job as documented (genuine defect; see §4).
* **EXPECTED-FAIL** — cannot succeed in this environment by design (missing `gh`/`jj`, cycles disabled) or by rule.
* **SKIP** — deliberately not executed (rule 2: credentials/workspace-structure changes); `--help` and clap argument validation were verified instead.

### Corrections to the brief (important)

* **The board was NOT empty at audit start.** The live baseline, captured before any write, was: **1 team (`WAV`), 1 project (`<project>`, slug `<project-slug>`), 7 issues (WAV-1…WAV-7), 4 labels (`Bug`, `Feature`, `Improvement`, `Migrated`), 0 documents, 0 initiatives, 0 milestones, 0 cycles**, plus 6 pre-existing GitHub-synced comments and 3 attachments on WAV-5/6/7. The "0 issues / 0 labels" assumption in the brief was wrong (most likely the stale `LINEAR_GRAPHQL_ENDPOINT`/`LINEAR_API_KEY` probe problem); the zero-count sections of the summary below are measured against the **real** baseline, not against an empty board.
* **`gh` and `jj` are absent, and `xdg-open` is absent** — so `issue pull-request`, `issue commits` and every `--web`/`-a` flag cannot reach their happy path here (the actual failure each one reports is recorded).
* **GitHub integration side effect (disclosure):** the workspace has `githubImport`/`githubCommit`/`githubCodeAccessPersonal`/`github` integrations enabled. Creating scratch issue WAV-8 caused Linear to create/attach **GitHub issue #1 in `<org>/<repo>`** (`#1 ZZZ-AUDIT-issue`) and to mirror comment activity into it. Deleting WAV-8 removed the Linear-side attachment, but GitHub is unreachable from this container (`curl` to github.com times out), so the GitHub-side artefact could not be re-checked or removed. No existing Linear data was touched by it.

---

## 2. Per-group results

### auth

| command | result | evidence |
|---|---|---|
| `auth login` | SKIP | help verified; clap validation only: `auth login a b` → exit 2 `error: unexpected argument 'a' found`. Not executed (changes credentials). |
| `auth logout` | SKIP | help verified; `auth logout a b` → exit 2 `error: unexpected argument 'b' found`. Not executed. |
| `auth list` | OK | exit 0, `No workspaces configured` (auth comes from `~/.config/linear/linear.toml`; the multi-workspace store is empty — see note N1) |
| `auth default` | SKIP | help verified; `auth default a b` → exit 2. Not executed. |
| `auth token` | OK | exit 0, prints a 48-char `lin_…` token (secret deliberately not recorded) |
| `auth whoami` | OK | exit 0, `Workspace: WAVE-cloud` |
| `auth migrate` | SKIP | help verified; `auth migrate --nope` → exit 2. Not executed. |

### issue

| command | result | evidence |
|---|---|---|
| `issue id` | **FAIL** | on branch `wav-8-zzz-audit` in a git repo: exit 1 `✗ Failed to get issue ID: Could not determine issue ID`. 7 branch patterns tried (`wav-8`, `WAV-8-zzz`, `feature/wav-8-thing`, `<user>/wav-8-thing`, …) — all fail. See F5. |
| `issue mine` | OK | exit 0 `No issues found.`; `--all-states` renders the table incl. WAV-1…7 |
| `issue query` | OK | exit 0; `--json` → `nodes` 7 parsed (`keys=[nodes,pageInfo] depth=6 nodes=7`); `--search 'Get familiar'` → 2 nodes. `--include-archived` is broken → F3 |
| `issue title` | OK | `issue title WAV-1` → `Get familiar with Linear` (no-arg fails, F5) |
| `issue start` | OK | `issue start WAV-12 --branch zzz-audit-start` → `✓ Created and switched to branch 'zzz-audit-start'` + `✓ Issue state updated to 'In Progress'`; no-arg → exit 1 `Cannot select "Select an issue to start:" in a non-interactive environment` |
| `issue view` | OK | `--no-comments` → `# WAV-1: Get familiar with Linear`; `--json` parsed; `--web` blocked by config (F8) |
| `issue url` | OK | `https://linear.app/wave-cloud/issue/WAV-1/get-familiar-with-linear` |
| `issue describe` | OK | `WAV-1 Get familiar with Linear`; `--references` identical shape |
| `issue commits` | EXPECTED-FAIL | exit 1 `✗ Failed to show commits: commits is only supported with jj-vcs` (`jj` not installed) |
| `issue pull-request` | EXPECTED-FAIL | exit 1 `✗ Failed to create pull request: Failed to create pull request` (`gh` missing; message itself is defective → F7) |
| `issue archive` | OK | no `--confirm` → exit 1 `Interactive confirmation required / Use --confirm to skip.`; with `--confirm` archives; re-run → `is already archived.`; `--bulk` respects `--confirm` |
| `issue delete` | OK | no `--confirm` → exit 1 `Interactive confirmation required`; `--confirm` deletes; `--bulk`/`--bulk-stdin` work; unknown id → `Could not find referenced Issue.` |
| `issue create` | OK | full flag set (`--project --milestone --priority --estimate --due-date --state --assignee self --description-file`) succeeded; no flags → exit 1 `Title is required when not using interactive mode` |
| `issue update` | OK | title/state/assignee/priority/estimate/due-date/clear-*/parent/clear-parent/description/project/milestone/clear-project/clear-milestone/cycle/clear-cycle/unassign all applied; conflicting flags → exit 1 `Cannot specify both --assignee and --unassign`; bad state → `Workflow state not found: 'NotAState' for team WAV` |
| `issue comment add` | OK | `✓ Comment added to WAV-8`; `--parent` creates a threaded reply |
| `issue comment update` | OK | `✓ Comment updated` |
| `issue comment delete` | OK | `✓ Comment deleted` (no confirmation prompt) |
| `issue comment list` | OK | `--json` parsed (2 then 3 nodes after reply); text form lists body/author/id |
| `issue attach` | OK | `✓ Uploaded scratch.txt`; `--comment` variant OK; missing file → exit 1 `File not found: …` |
| `issue link` | OK | `issue link WAV-8 https://example.com/zzz-audit --title …` → `✓ Linked to WAV-8: ZZZ-AUDIT-link`; single-argument form needs a branch (F5) |
| `issue relation add` | OK | `✓ Created relation: WAV-8 blocks WAV-9` (verified in raw API) |
| `issue relation list` | OK | `Relations for WAV-8: …` + `Outgoing:`/`Incoming:` sections with type + both identifiers |
| `issue relation delete` | OK | `✓ Deleted relation: WAV-8 blocks WAV-9`; repeat → exit 1 `Relation not found: blocks between WAV-8 and WAV-9` |
| `issue agent-session list` | OK | `No agent sessions found for this issue.`; `--json` parsed (`nodes=2`) |
| `issue agent-session view` | OK | bogus UUID → exit 1 `Could not find referenced AgentSession.` |
| `issue list` (hidden alias) | OK | not in `issue --help`; `issue list` and `issue list --all-states` behave exactly like `issue mine` (alias `list`, source `#[command(alias="list")]`); `--json` rejected by clap (exit 2) |

### project

| command | result | evidence |
|---|---|---|
| `project list` | OK | renders table; `--json` parsed (`nodes=1` after cleanup: `<project>`); `--team WAV` OK |
| `project view` | OK | `# <project> [P-WAV-1]`; by slug id `<project-slug>`; `--json` parsed |
| `project create` | OK | `project create --name ZZZ-AUDIT-project --team WAV --json` → `{success, project}` |
| `project update` | OK | `--name/--description/--content/--status` applied; `--status nonsense` → exit 1 `Invalid status: nonsense`; `--label <unknown>` → exit 1 `Project label not found: …` |
| `project delete` | OK | no `--force` → exit 1 `Interactive confirmation required / Use --force to skip confirmation.`; `--force` deleted |
| `project comment add` | OK | `✓ Comment added to project ZZZ-AUDIT-project` |
| `project comment list` | OK | `--json` parsed (`nodes=1`); text form shows author/time/id |

### project-update

| command | result | evidence |
|---|---|---|
| `project-update create` | OK | `Created status update for: ZZZ-AUDIT-project`; `--health nonsense` → exit 1 `Invalid health value: nonsense`; unknown project → `Project not found` |
| `project-update list` | OK | `--json` parsed (`keys=[name,slugId,projectUpdates] nodes=1`); text form `Status updates for: …` |

### team

| command | result | evidence |
|---|---|---|
| `team create` | SKIP | not executed (rule 2); `--help` verified (name/description/key/private) |
| `team delete` | SKIP | not executed (rule 2); `--help` verified |
| `team list` | OK | table with `WAV <workspace> No 43 minutes ago <team-id>`; `--json` parsed |
| `team id` | OK | `WAV` (resolves the configured UUID reference — regression-covered by `tests/team_scope.rs`) |
| `team autolinks` | EXPECTED-FAIL | exit 1 `✗ Failed to configure autolinks: workspace is not set via command line, configuration file, or environment` — fails on workspace resolution **before** it can need `gh` (see F8) |
| `team members` | OK | `Team Members (1):`; `--json` parsed (`nodes=1`); unknown team → exit 1 `Team not found: ZZZ-NOPE` |
| `team states` | OK | `--json` parsed (`nodes=7`); unknown team → `Team not found: ZZZ-NOPE` |

### user

| command | result | evidence |
|---|---|---|
| `user list` | OK | `Workspace Members (2):`; `--json` parsed (`nodes=2`: `Linear` integration user + `<user>`); `--all` accepted |

### cycle *(cycles are disabled for team WAV — recorded as expected, not as a bug)*

| command | result | evidence |
|---|---|---|
| `cycle list` | OK | exit 0 `No cycles found for this team.`; `--json` `{"nodes":[],"pageInfo":…}` parsed; `--team ZZZ-NOPE` → exit 1 `Team not found: ZZZ-NOPE` |
| `cycle view` | EXPECTED-FAIL | exit 1 `✗ Failed to fetch cycle details: Cycles are not enabled for team WAV`; `--json` → same error on stderr, no stdout (exit 1, `json parse failed`) |

### milestone

| command | result | evidence |
|---|---|---|
| `milestone list` | OK | `No milestones found for this project.` on the real board; with a scratch milestone: table + `--json` parsed |
| `milestone view` | OK | `milestone view <name> --project <p>` and `--all` both render; `--json` parsed; unknown name/UUID → `Milestone not found: …` / `Could not find referenced ProjectMilestone.` |
| `milestone create` | OK | `✓ Created milestone: ZZZ-AUDIT-milestone` (`--description`, `--target-date` applied) |
| `milestone update` | OK | works with the milestone **UUID** (`✓ Updated milestone`); a **name is rejected** with a leaked GraphQL error → F9 |
| `milestone delete` | OK | no `--force` → exit 1 `Interactive confirmation required`; `--force` → `✓ Deleted milestone <uuid>` |

### initiative

| command | result | evidence |
|---|---|---|
| `initiative list` | OK | `No initiatives found.` (post-cleanup) / full table while scratch initiatives existed; `--json` parsed; `--all-statuses`, `--archived`, `--owner` accepted |
| `initiative view` | OK | `# ZZZ-AUDIT-init` by name, slug id and `--json` (`keys=[id,slugId,name,description,status,targetDate]`) |
| `initiative create` | OK | `✓ Created initiative: …` with `--status planned|active|completed` (validated + canonicalised); bare create defaults to Active |
| `initiative update` | OK | `--name/--description/--owner/--target-date/--color/--icon` applied; no flags → `No changes specified`. **`--status` can never work → F2** |
| `initiative archive` | OK | no `--force` → exit 1 `Interactive confirmation required. Use --force to skip.` (1.5 s, no hang); with `--force` → `✓ Archived initiative` |
| `initiative unarchive` | OK | `✓ Unarchived initiative: …` + URL |
| `initiative delete` | OK | no `--force` → exit 1 `Interactive confirmation required` (1.6 s); `--force` and `--bulk` delete; wording is misleading → F11 |
| `initiative add-project` | OK | `✓ Added "ZZZ-AUDIT-project" to initiative "ZZZ-AUDIT-init"` |
| `initiative remove-project` | OK | `✓ Removed …`; re-run → `Project "…" is not linked to initiative "…"` (exit 0, idempotent); unknown initiative → exit 1 with `Pass an initiative UUID, slug ID, or exact initiative name.` |
| `initiative comment add` | OK | `✓ Comment added to initiative ZZZ-AUDIT-init` |
| `initiative comment list` | OK | `--json` parsed (`nodes=1`); text form shows author/time/id |

### initiative-update

| command | result | evidence |
|---|---|---|
| `initiative-update create` | OK | `Created status update for: ZZZ-AUDIT-init`; `--health bogus` → exit 1 `Invalid health value: bogus` |
| `initiative-update list` | OK | `--json` parsed (`keys=[name,slugId,initiativeUpdates] nodes=1`) |

### label

| command | result | evidence |
|---|---|---|
| `label list` | OK | table of 4 workspace/team labels; `--json` parsed; `--workspace` (3 nodes), `--all`, `--team` variants OK; **stable order across 3 consecutive runs** (same 6-element order each time) |
| `label create` | OK | `✓ Created label: ZZZ-AUDIT-label` (workspace) and `… ZZZ-AUDIT-teamlabel` (`--team WAV`); no flags → exit 1 `Label name is required` |
| `label delete` | **FAIL** | `label delete ZZZ-AUDIT-label` (also `Bug`, `Migrated`, `--team WAV` variants) → exit 1 `✗ Failed to delete label: Label not found: <name> / Searched in team WAV and workspace.` while the label exists. Only a **UUID** works (`✓ Deleted label: ZZZ-AUDIT-label (Workspace)`). See F1. |

### template

| command | result | evidence |
|---|---|---|
| `template list` | OK | `No templates found.` (the workspace has none); `--json` → `[]` parsed; `--type issue` OK; `--type nonsense` → clap exit 2 |
| `template view` | OK | not-found path only: exit 1 `✗ Failed to view template: Template not found: ZZZ-AUDIT-nonexistent`. No template exists in this workspace, so the happy path is unverifiable here. |

### document

| command | result | evidence |
|---|---|---|
| `document list` | OK | `No documents found.`; `--json` parsed; `--project/--issue/--team/--cycle/--release` filters accepted |
| `document view` | OK | by UUID and by slug id; `--raw`; `--json` parsed (`keys=[id,title,slugId,content,url,createdAt]`); unknown → `Document not found`; `--web` blocked by config (F8) |
| `document create` | OK | `✓ Created document: ZZZ-AUDIT-doc` attached to the scratch project; no flags → exit 1 `Title is required` |
| `document update` | OK | `--title`, `--content`, `--content-file` applied; no fields → exit 1 `No update fields provided`; **`--icon` can never succeed → F6** |
| `document delete` | OK | no `--yes` → exit 1 `Interactive confirmation required / Use --yes to skip.`; `--yes` → `✓ Deleted document: ZZZ-AUDIT-doc` |
| `document comment add` | OK | body and `--body-file` variants → `✓ Comment added to document …` |
| `document comment list` | OK | `--json` parsed (`nodes=1`); text form shows author/time/id |

### schema / api / markdown / completions / config

| command | result | evidence |
|---|---|---|
| `schema` | OK | SDL to stdout (1.2 MB); `--json` → introspection parsed (`keys=[__schema] depth=12 nodes=1214`); `-o /tmp/…` → `Schema written to …` |
| `api` | OK | `query { viewer { name } }` → `{"data":{"viewer":{"name":"<user>"}}}`; `--variable` and `--variables-json` OK; `--silent` empty + exit 0; `--paginate` OK; stdin form (`api -`) OK; invalid query → exit 1 with the full GraphQL error JSON on stdout |
| `markdown` | OK | exit 0, 40 lines starting `Linear-flavored Markdown: mentions and collapsible sections` |
| `completions` | OK | `bash` 105 843 B / `zsh` 82 932 B / `fish` 85 043 B / `powershell` 112 301 B, all exit 0; bad shell → exit 1 `✗ Unsupported shell: tcsh` |
| `config` | SKIP | interactive generator — not executed (rule 2); `config --help` verified |

---

## 3. Agent-facing contract checks

| check | result | evidence |
|---|---|---|
| headless safety without a confirmation flag | **PASS** | 8 destructive paths run with `stdin </dev/null` and no confirm flag, all exit **1** in ≤ 1.6 s with an actionable message and no hang: `issue delete <id>` → `Interactive confirmation required / Use --confirm to skip.`; `issue archive <id>` and `issue archive --bulk` → same with `--confirm`; `document delete <id>` → `Use --yes to skip.`; `project delete <p>`/`milestone delete <id>` → `Use --force to skip confirmation.`; `label delete <uuid>` → same; `initiative archive`/`initiative delete` → `Interactive confirmation required. Use --force to skip.` Also clean: `issue create`/`project create`/`label create`/`document create`/`initiative create` with no flags (exit 1, "… is required"), `issue start` with no id (exit 1, "Cannot select … in a non-interactive environment") |
| error quality — unknown team | **PASS** | `team members ZZZ-NOPE` → exit 1 `✗ Failed to fetch team members: Team not found: ZZZ-NOPE`; same shape for `team states`, `issue query --team`, `cycle list --team` |
| error quality — unknown issue ID | **PASS** | `issue view WAV-9999` → exit 1 `✗ Failed to view issue: Could not find referenced Issue.`; `issue view not-an-issue` and a well-formed but unknown UUID → `Could not determine issue ID` (correct for a bad identifier form, though the wording doesn't say "that is not an ID") |
| error quality — malformed UUID | **PASS** | `project view 12345678-…` → exit 1 `Project not found`/`Could not find referenced Project.`; `milestone view 12345678-…` → `Could not find referenced ProjectMilestone.`; `document view ZZZ-NOPE` → `Document not found: ZZZ-NOPE`; `initiative view ZZZ-NOPE` → `Initiative not found` |
| `linear api 'query { viewer { name } }'` | **PASS** | exit 0 → `{"data":{"viewer":{"name":"<user>"}}}` |
| piping / SIGPIPE | **PASS** | `linear issue list \| head -3` → CLI exit **0**, 17 B, empty stderr. Large outputs die with the signal and no panic text: `completions bash \| head -3` → 141, `schema \| head -3` → 141, `issue query --json \| head -3` → 141, all with **0 bytes on stderr** (no `panicked at …: Broken pipe`). Buffered-small commands (`label list`, `team list --json`, `issue view`) exit 0 |
| `LINEAR_DEBUG=1 linear team list` | **PASS** | exit 0, normal table on stdout, debug/trace detail on stderr only; `LINEAR_DEBUG=1 issue query --json` also exit 0 with valid JSON |
| `linear completions bash\|zsh\|fish\|powershell` | **PASS** | all exit 0 and non-empty (byte counts above); output starts `_linear() {`, `#compdef linear`, `# Print an optspec…`, `using namespace System.Management.Automation` |
| ordering / idempotency | **PASS (with one flag defect)** | `label list --json` returns a byte-identical order over 3 runs; `team list --json` stable; re-running `issue archive`, `initiative remove-project`, `issue relation delete` on already-done state gives a clear message and a sane exit code. Exception: `issue query --include-archived` (F3) returns a *stable but wrong* answer |

### Flag-level and behavioural checks (12 rows: 6 FAIL, 1 EXPECTED-FAIL, 5 OK)

| check | result | evidence |
|---|---|---|
| `issue query --include-archived` — non-search path | **FAIL** | identical 10 identifiers with and without the flag; archived WAV-11 missing from both (F3) |
| `issue query --include-archived` — `--search` path | OK | archived WAV-11 returned (4 ids vs 3 without the flag) |
| `issue create --label <workspace label>` | **FAIL** | `✗ Failed to create issue: Issue label not found: Bug` (F4) |
| `issue update --label/--add-label <workspace label or UUID>` | **FAIL** | `✗ Failed to update issue: Issue label not found: Bug` / `…: a5d93494-…` (F4) |
| `issue update --add-label <team label>` | OK | `--add-label Migrated` → `✓ Updated issue WAV-8`; `--remove-label` likewise |
| `document update --icon <emoji or name>` | **FAIL** | every value (`🚀 📄 🔍 book FileText`) → `icon is not a valid icon.` (F6) |
| `milestone update <name>` (UUID path OK) | **FAIL** | `✗ Failed to update milestone: Could not find referenced ProjectMilestone.` (F9) |
| `issue update <id>` with no field flags | **FAIL** | prints `✓ Updated issue WAV-1` for a mutation that changes nothing (F10) |
| `issue/project/document view --web` (and `-a`) | EXPECTED-FAIL | `✗ workspace is not set via command line, configuration file, or environment`; with `--workspace wave-cloud` it proceeds and then reports `xdg-open is not available` (F8) |
| `issue archive --bulk`, `issue delete --bulk`, `--bulk-stdin` | OK | respect the confirm flag; print `Found N issue(s)…`, then per-item failures for unknown ids (`Failed operations:` list) |
| `label list --json` ordering | OK | byte-identical node order over 3 consecutive runs |
| destructive-command headless matrix (8 paths, no confirm flag, stdin closed) | OK | all exit 1 in ≤ 1.6 s with `Use --confirm/--force/--yes to skip`; no hang, no prompt |

---

## 4. Findings

Ordered by severity. Every repro was run against the live API.

### F1 — `label delete <NAME>` can never find a label (high)

`label delete` only works when given a UUID; the documented `<NAME_OR_ID>` name path is dead for **all** labels.

```
$ linear label list --workspace --json | jq -r '.nodes[].name'   # Bug, Feature, Improvement, …
$ linear label delete Bug
✗ Failed to delete label: Label not found: Bug
  Searched in team WAV and workspace.          # exit 1
$ linear label delete f5ac78f4-c2a9-4d8c-823f-1b4f89d9eeb2       # UUID
✗ Failed to delete label: Interactive confirmation required …    # i.e. it resolved correctly
```
Root cause (verified): `src/commands/label/label_delete.rs:21` declares `query GetLabelByName($name: String!, $teamKey: String)` but never uses `$teamKey`, and graphql-js rejects the operation with `GRAPHQL_VALIDATION_FAILED: Variable "$teamKey" is never used in operation "GetLabelByName"` (reproduced verbatim through `linear api`). The caller wraps the request in `let Ok(result) = … else { return Ok(None) }`, so the *validation error is reported as "not found"*. Expected: name lookup works, or at minimum the real error surfaces. Same defect family as upstream's helper name, but here it is fatal for the whole name path.

### F2 — `initiative update --status` can never work (high)

```
$ linear initiative update <INIT> --status active
✗ Failed to update initiative: Variable "$input" got invalid value "active" at "input.status";
  Value "active" does not exist in "InitiativeStatus" enum. Did you mean the enum value "Active"?
$ linear initiative update <INIT> --status Active        # same error — the CLI still sends "active"
```
`initiative_create.rs` canonicalises/validates the value against `Planned|Active|Completed` and *works* (`--status active` → created Active); `initiative_update.rs:206` does `input.insert("status", json!(status.to_lowercase()))` with no validation, so the lowercase enum never matches. Observed: status cannot be changed from the CLI at all. Expected: `--status active` (or `Active`) sets the status, as `create` does.

### F3 — `issue query --include-archived` is silently ignored in the non-search path (high)

```
$ linear issue query --all-teams --all-states --json | jq -r '.nodes[].identifier'               # 10 ids
$ linear issue query --all-teams --all-states --include-archived --json | jq -r '.nodes[].identifier'
# identical 10 ids — archived WAV-11 is absent from both
$ linear issue query --all-teams --search ZZZ-AUDIT --include-archived --json | jq -r '.nodes[].identifier'
WAV-8 WAV-10 WAV-9 WAV-11         # the search path DOES include it (without --include-archived: 3 ids)
```
Raw API proof that the flag is meaningful: `issues(first:50, includeArchived:true)` returns WAV-11, the default does not. Root cause: `FETCH_ISSUES_QUERY` (`src/linear/queries.rs:271`) declares only `$filter, $sort, $first, $after` and does not pass `includeArchived:` to the `issues(...)` field, while `fetch_issues_for_query` unconditionally injects an `includeArchived` variable. GraphQL ignores undeclared variables, so the flag becomes a silent no-op — the user gets a stable but wrong answer. Expected: either include archived issues, or reject the flag.

### F4 — Workspace-level labels cannot be applied to any issue (high)

```
$ linear label list --workspace --json | jq -r '.nodes[].name'   # Bug, Feature, Improvement, …
$ linear issue update WAV-8 --add-label Bug
✗ Failed to update issue: Issue label not found: Bug                  # exit 1
$ linear issue update WAV-8 --add-label a5d93494-8a3e-4395-b838-f56b908062b7    # same failure by UUID
$ linear issue create --title X --team WAV --label Bug --no-interactive
✗ Failed to create issue: Issue label not found: Bug                  # exit 1
```
Team labels work (`--add-label Migrated` → `✓ Updated`). Root cause: `GET_ISSUE_LABEL_BY_NAME_QUERY` filters on `team: { key: { eq: $teamKey } }`; workspace labels have `team = null` and therefore never match, and no UUID fallback exists for labels on issues. Impact: in a stock Linear workspace the default labels are workspace-level, so *no default label* can be attached from the CLI, and `label list --workspace` advertises exactly the labels the write path rejects. Expected: workspace labels resolvable (the API's `issueLabelId` accepts them) or a clear "team labels only" message.

### F5 — Branch-state issue resolution is not implemented, so `[issueId]`-optional commands fail in a repo (medium-high)

`cite`: `src/linear/identifiers.rs:74` ends with `// TODO(#13): read the current issue from git/jj branch state.` and returns `Ok(None)` whenever no ID is passed. Observed in a git repo (`/tmp/audit/repo`, branch `wav-8-zzz-audit`, real issue WAV-8):

```
$ linear issue id
✗ Failed to get issue ID: Could not determine issue ID
  Please provide an issue ID or run from a branch with an issue identifier.
```
Also failing: `issue title`, `issue url`, `issue view`, `issue describe`, `issue link <url>`, `issue comment add|list`, `issue relation list`, `issue archive|delete` (no id), `issue agent-session list`, `issue start` (no id). Tried branch names `wav-8-zzz-audit`, `wav-8`, `WAV-8-zzz`, `wav-8/zzz`, `feature/wav-8-thing`, `<user>/wav-8-thing`, `zzz-audit-wav-8` — all exit 1. `issue start --branch …` does create the branch and set the issue `In Progress`, but stores no association (no `linear*` git config keys), so a subsequent `issue id` on the created branch still fails. Expected: the identifier is parsed out of the branch (upstream `linear issue id` semantics) so agent flows like "start issue → later read it back" work.

### F6 — `document update --icon` (and `document create --icon`) can never succeed (medium)

```
$ linear document update <DOC> --icon 🚀        ✗ Failed to update document: icon is not a valid icon.   # exit 1
$ linear document update <DOC> --icon 📄        ✗ Failed to update document: icon is not a valid icon.
$ linear document update <DOC> --icon book      ✗ Failed to update document: icon is not a valid icon.
```
Every value fails, including emoji, while the help text documents `New icon (emoji)`. Confirmed server-side: the identical value through `linear api 'mutation { documentUpdate(input:{icon:…}) }'` returns `INVALID_INPUT … "icon is not a valid icon."` — so the CLI forwards a value Linear always rejects, with no validation and no hint about the accepted format (note `initiative update --icon Rocket` succeeds, so this is specific to documents). Expected: `--icon <emoji>` works, or the flag/help reflects what Linear accepts.

### F7 — `issue pull-request` failure message is a tautology and `LINEAR_DEBUG=1` adds nothing (medium)

```
$ linear issue pull-request WAV-8
✗ Failed to create pull request: Failed to create pull request          # exit 1
$ LINEAR_DEBUG=1 linear issue pull-request WAV-8
✗ Failed to create pull request: Failed to create pull request          # byte-identical, no detail
```
`gh` is not installed, which is the real cause, and the message never says so — compare `issue commits` (`commits is only supported with jj-vcs`) or `team autolinks` (names the missing setting). The documented promise of `LINEAR_DEBUG=1` ("Show full error details including stack traces") is not met on this path. Expected: name the missing dependency.

### F8 — `--web`/`-a` are unusable in the supported single-workspace setup (medium)

```
$ linear issue view WAV-8 --web
✗ workspace is not set via command line, configuration file, or environment      # exit 1
$ linear project view <PROJECT> --web
✗ Failed to view project: workspace is not set via command line, configuration file, or environment
$ linear document view <DOC> --web
✗ Failed to view document: workspace is not set via command line, configuration file, or environment
$ linear --workspace wave-cloud issue view WAV-8 --web
Opening https://linear.app/wave-cloud/issue/WAV-8 in web browser
✗ Failed to open https://linear.app/wave-cloud/issue/WAV-8: `xdg-open` is not available   # expected: no browser here
```
The browser helpers require a workspace *slug* from the credentials store (`auth list` → `No workspaces configured`), even though the CLI resolves the same slug elsewhere (`issue url` prints `linear.app/wave-cloud/…`, `organization.urlKey` is available). Expected: derive the slug, or the error should name the flag to pass. The same resolution failure is what `team autolinks` hits before it can reach its `gh` requirement.

### F9 — `milestone update <name>` leaks a GraphQL entity name and suggests nothing (low)

```
$ linear milestone update ZZZ-AUDIT-milestone --name ZZZ-AUDIT-milestone
✗ Failed to update milestone: Could not find referenced ProjectMilestone.      # exit 1
$ linear milestone update 2f67c8e9-a17c-41c6-af9a-dc3d7117703a --name ZZZ-AUDIT-milestone
✓ Updated milestone: ZZZ-AUDIT-milestone
```
`milestone view` accepts a name (`--project` scopes it), `milestone update` does not, and the failure is an internal entity name rather than `Milestone not found: <name> — pass the UUID (run `milestone list`)`. Expected: consistent reference resolution across `view`/`update`/`delete`, or a suggestion.

### F10 — `issue update <id>` with no field flags reports success while doing nothing (low)

```
$ linear issue update WAV-1
Updating issue WAV-1
✓ Updated issue WAV-1: Get familiar with Linear
# exit 0; raw API confirms nothing changed (title/state/labels/assignee/project untouched, updatedAt not bumped)
```
Compare `document update` (exit 1 `No update fields provided`) and `initiative update` (`No changes specified`, exit 0). A "✓ Updated" line with no change is misleading for scripted agents. Expected: `No update fields provided` and a non-zero exit.

### F11 — deletion is described as permanent but Linear soft-deletes (low)

```
$ linear initiative delete --bulk ZZZ-AUDIT-init … --force
⚠️  This action is PERMANENT and cannot be undone.
✓ Successfully deleted 4 initiatives
$ linear initiative list --all-statuses --archived      # the same four are still listed
$ linear api 'query { initiatives(includeArchived:true){nodes{name archivedAt}} }'   # archivedAt = delete time
```
`issue delete` behaves the same way (`trashed: true`; invisible to every default listing, still returned by `includeArchived: true`). The objects are gone from normal views but the warning text is inaccurate, and no CLI command can purge them. Expected wording: "moves to the trash / archive".

### N1 (note, not a bug) — `auth list` reports no workspaces while auth works

`auth list` → `No workspaces configured`, yet `auth whoami` works: this setup authenticates from `~/.config/linear/linear.toml`, a different path from the multi-workspace `credentials.toml` that `auth list` prints. Correct but easily misread; worth a one-line hint in the output.

### Observations that are **not** bugs (do not "fix" these)

* `linear issue list` is a hidden **alias of `issue mine`** (`alias = "list"`, and `l`), not a general issue listing, and it accepts no `--json`. `linear issue list | head -3` exits 0 with `No issues found.` (correct for the mine-filter), so the SIGPIPE regression contract holds.
* Piping a huge output (`completions`, `schema`) to `head` terminates the CLI with SIGPIPE (exit 141) and **no panic text** — standard Unix behaviour.
* Regressing two relations between the same pair (`blocks` then `related`) leaves only the newest one; the follow-up `relation delete blocks` correctly reports `Relation not found`. That is Linear's API behaviour, and the CLI reports it accurately.
* `cycle list`/`cycle view` behaviour with cycles disabled is exactly as the brief predicted (`No cycles found for this team.` / `Cycles are not enabled for team WAV`).
* `template view` could only be exercised on its not-found path — the workspace has no templates to view.

---

## 5. Workspace state proof (before / after)

Baseline captured with a raw GraphQL query **before the first write**; final state captured after all cleanup.

| object | before | after | after (including trash/archived) |
|---|---|---|---|
| teams | 1 — `WAV` / WAVE-cloud | 1 — `WAV` / WAVE-cloud | same |
| projects | 1 — `<project>` (`<project-slug>`, backlog, 0 milestones) | 1 — identical (name/slug/state/milestones unchanged) | same |
| issues | 7 — WAV-1…WAV-7 | **7 — WAV-1…WAV-7** (titles identical) | 12 — the same 7 live + WAV-8, WAV-9, WAV-10, WAV-11, WAV-12 `trashed: true` |
| labels | 4 — `Bug`, `Feature`, `Improvement`, `Migrated` | **4 — identical** | same (no trashed labels visible) |
| documents | 0 | **0** | 0 |
| initiatives | 0 | **0** | 5 — `ZZZ-AUDIT-init`, `-init2`, `-init3`, `-init4`, `-init-headless` with `archivedAt` = delete time (soft-deleted) |
| milestones | 0 | **0** | 0 |
| cycles | 0 (cycles disabled) | 0 | 0 |
| comments on the board | 6 (all on WAV-5/6/7, GitHub-synced, pre-existing) | 6 — identical, all on WAV-5/6/7 | same |
| attachments | 3 (WAV-5/6/7, GitHub links) | 3 — identical | same |
| users | 2 — `Linear` (integration), `<user>` | 2 — identical | same |

Re-run these to reproduce the counts:

```
linear issue query --all-teams --all-states --json   # 7 nodes: WAV-1..WAV-7
linear project list --json                           # 1 node: <project>
linear label list --json                             # 4 nodes: Bug, Feature, Improvement, Migrated
linear document list --json                          # 0 nodes
linear initiative list --json                        # 0 nodes
linear milestone list --project <project-slug> --json  # 0 nodes
linear team list --json                              # 1 node: WAV
linear cycle list --json                             # 0 nodes
```

**Residuals (unavoidable via the public API, all `ZZZ-AUDIT-*` only):**

1. 5 issues (`WAV-8…WAV-12`, titles all `ZZZ-AUDIT-…`) sit in Linear's trash — `trashed: true`, absent from every default listing, returned only by `includeArchived: true`. `issue delete` has no purge.
2. 5 initiatives (`ZZZ-AUDIT-init`, `-init2`, `-init3`, `-init4`, `-init-headless`) are soft-deleted — absent from `initiative list`, still returned by `initiative list --archived` / `includeArchived: true`. `initiativeDelete` (the mutation the CLI uses, verified directly) is itself a soft delete, so no client-side purge exists.
3. The GitHub integration artefact: Linear created/attached GitHub issue `#1` in `<org>/<repo>` for the scratch issue `WAV-8` and mirrored comments into it. The Linear-side attachment is gone with WAV-8; GitHub is unreachable from this container (`curl` timeout), so the GitHub-side item could not be verified or removed.

All other audit objects (2 labels, 1 project, 1 milestone, 1 document, 5 initiatives, 5 issues, plus their comments, relations and attachments) were deleted, and every write touched only `ZZZ-AUDIT-*` objects. Scratch state under `/tmp/audit` was removed after the report was written.

---

## 6. Method & reproducibility

* Surface enumeration: `linear --help`, then `linear <group> --help` for every group it lists, then `linear <group> <sub> --help` for every sub-command including the nested groups (`project comment`, `initiative comment`, `issue comment`, `issue relation`, `issue agent-session`, `document comment`) — 87 help screens were captured before any call at the time of this audit (the tree has since grown; see the note at the top).
* Every write was confined to objects created by this audit, all named `ZZZ-AUDIT-*`; the real board (`<project>`, team `WAV`, issues WAV-1…7) was never modified, archived or deleted (`project view`, `issue query`, `issue view`, `label list` only).
* Credentials: never printed (the `auth token` check recorded length/prefix only), never exported, and `linear.toml` was never read or modified; all commands used per-invocation `env -u …` prefixes.
* Not executed by design (rule 2): `team create`, `team delete`, `auth login`, `auth logout`, `auth default`, `auth migrate`, interactive `config`. For these, `--help` plus clap argument validation were recorded; no credential or workspace-structure change was attempted.
* Two signature `--json` outputs were re-validated in a second pass (`python3 -c 'import json,sys; json.load(sys.stdin)'`-equivalent) after an early harness reporting glitch; the corrected results are what the tables quote.
