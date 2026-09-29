//! End-to-end tests for the `linear initiative` group (`list`, `view`,
//! `unarchive`, and the nested `comment add` / `comment list`), run against the
//! headless mock server.
//!
//! Port of upstream `test/commands/initiative/`:
//! `initiative-list.test.ts`, `initiative-view.test.ts`,
//! `initiative-unarchive.test.ts`, `initiative-comment.test.ts`,
//! `initiative-comment-add.test.ts`, `initiative-comment-list.test.ts`
//! (upstream holds no snapshot file for the unarchive suite).
//!
//! Upstream drives these through `@cliffy/testing` snapshot tests; here they are
//! plain assertion tests against the strings this port actually prints. That is
//! deliberate: the port has no `@littletof/charmd` renderer, its help text is
//! clap's (not cliffy's), and its dev-dependencies do not include a snapshot
//! crate. Wherever the port's text, request shape, or ordering differs from
//! upstream's snapshot, the test follows the *port* and the divergence is noted
//! inline.

mod common;

use common::{run_cli, MockLinearServer, MockResponse};
use serde_json::json;

/// A valid Linear UUID short-circuits initiative resolution, so no resolver
/// mock is needed — the same shortcut `tests/initiative_update.rs` takes.
const INITIATIVE_ID: &str = "11111111-1111-1111-1111-111111111111";

/// The two initiatives `initiative-list.test.ts` serves, in the order the mock
/// returns them (reversed relative to the port's sort, which is the point).
fn get_initiatives_response() -> serde_json::Value {
    json!({ "data": { "initiatives": {
        "nodes": [
            {
                "id": "initiative-2",
                "slugId": "plan-b",
                "name": "Plan B",
                "description": "Second initiative",
                "status": "Planned",
                "targetDate": "2026-06-01",
                "health": "atRisk",
                "color": "#f59e0b",
                "icon": "🟡",
                "url": "https://linear.app/test/initiative/plan-b",
                "archivedAt": null,
                "owner": { "id": "owner-2", "displayName": "Pat Planner", "initials": "PP" },
                "projects": { "nodes": [
                    { "id": "project-2", "name": "Project B", "status": { "name": "Planned" } }
                ] }
            },
            {
                "id": "initiative-1",
                "slugId": "alpha",
                "name": "Alpha",
                "description": "First initiative",
                "status": "Active",
                "targetDate": "2026-05-01",
                "health": "onTrack",
                "color": "#10b981",
                "icon": "🟢",
                "url": "https://linear.app/test/initiative/alpha",
                "archivedAt": null,
                "owner": { "id": "owner-1", "displayName": "Alex Active", "initials": "AA" },
                "projects": { "nodes": [
                    { "id": "project-1", "name": "Project A", "status": { "name": "In Progress" } }
                ] }
            }
        ],
        "pageInfo": { "hasNextPage": false, "endCursor": null }
    } } })
}

/// `initiative-list.test.ts` — "Initiative List Command - JSON Output".
///
/// Upstream pins `variables: { filter: undefined, includeArchived: false }`;
/// the harness cannot express "undefined", and `--all-statuses` makes the port
/// omit `filter` entirely, so only `includeArchived` is pinned here.
///
/// Upstream's snapshot is the whole connection: both nodes, sorted Active
/// before Planned even though the mock returns them the other way round. The
/// port sorts by status order then name (the snapshot's order matches), so the
/// ordering is asserted rather than assumed.
#[test]
fn initiative_list_json_output() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "GetInitiatives",
        get_initiatives_response(),
    )
    .with_variables(json!({ "includeArchived": false }))]);

    let out = run_cli(
        &["initiative", "list", "--all-statuses", "--json"],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);

    let parsed: serde_json::Value = serde_json::from_str(&out.stdout).expect("json output");
    assert_eq!(parsed["pageInfo"]["hasNextPage"], false);
    assert_eq!(parsed["nodes"][0]["id"], "initiative-1");
    assert_eq!(parsed["nodes"][1]["id"], "initiative-2");
    assert_eq!(parsed["nodes"][0]["slugId"], "alpha");
    assert_eq!(parsed["nodes"][0]["name"], "Alpha");
    assert_eq!(parsed["nodes"][0]["status"], "Active");
    assert_eq!(parsed["nodes"][0]["health"], "onTrack");
    assert_eq!(parsed["nodes"][0]["owner"]["displayName"], "Alex Active");
    assert_eq!(parsed["nodes"][0]["owner"]["initials"], "AA");
    assert_eq!(parsed["nodes"][0]["archivedAt"], serde_json::Value::Null);
    assert_eq!(
        parsed["nodes"][0]["projects"]["nodes"][0]["name"],
        "Project A"
    );
    assert_eq!(
        parsed["nodes"][0]["projects"]["nodes"][0]["status"]["name"],
        "In Progress"
    );
}

/// `initiative-view.test.ts` — "Initiative View Command - JSON Output".
///
/// Upstream's snapshot is the raw initiative object, field names and nesting
/// verbatim; `--json` re-emits the GraphQL node unchanged, so the same fields
/// are asserted here.
#[test]
fn initiative_view_json_output() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "GetInitiativeDetails",
        json!({ "data": { "initiative": {
            "id": INITIATIVE_ID,
            "slugId": "alpha",
            "name": "Alpha Initiative",
            "description": "Top-level initiative description.",
            "status": "active",
            "targetDate": "2026-05-01",
            "health": "onTrack",
            "color": "#10b981",
            "icon": "🟢",
            "url": "https://linear.app/test/initiative/alpha",
            "archivedAt": null,
            "createdAt": "2026-01-01T10:00:00Z",
            "updatedAt": "2026-02-01T10:00:00Z",
            "owner": { "id": "owner-1", "name": "alex.active", "displayName": "Alex Active" },
            "projects": { "nodes": [{
                "id": "project-1",
                "slugId": "project-a",
                "name": "Project A",
                "status": { "name": "In Progress", "type": "started" }
            }] }
        } } }),
    )
    .with_variables(json!({ "id": INITIATIVE_ID }))]);

    let out = run_cli(
        &["initiative", "view", INITIATIVE_ID, "--json"],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);

    let parsed: serde_json::Value = serde_json::from_str(&out.stdout).expect("json output");
    assert_eq!(parsed["id"], INITIATIVE_ID);
    assert_eq!(parsed["slugId"], "alpha");
    assert_eq!(parsed["name"], "Alpha Initiative");
    assert_eq!(parsed["description"], "Top-level initiative description.");
    assert_eq!(parsed["status"], "active");
    assert_eq!(parsed["targetDate"], "2026-05-01");
    assert_eq!(parsed["health"], "onTrack");
    assert_eq!(parsed["url"], "https://linear.app/test/initiative/alpha");
    assert_eq!(parsed["archivedAt"], serde_json::Value::Null);
    assert_eq!(parsed["createdAt"], "2026-01-01T10:00:00Z");
    assert_eq!(parsed["updatedAt"], "2026-02-01T10:00:00Z");
    assert_eq!(parsed["owner"]["displayName"], "Alex Active");
    assert_eq!(parsed["projects"]["nodes"][0]["slugId"], "project-a");
    assert_eq!(parsed["projects"]["nodes"][0]["status"]["type"], "started");
}

/// `initiative-unarchive.test.ts` — "initiative unarchive finds an archived
/// initiative by its URL".
///
/// The point of the test is the `includeArchived: true` on the slug lookup:
/// upstream's unarchive resolver calls
/// `findInitiativeIdBySlug(slugId, { includeArchived: true })` so that the one
/// command whose job is archived initiatives can address one by URL. The port
/// mirrors that through `linear::resolve_initiative_id_including_archived`, and
/// the mock below pins `includeArchived: true` — the harness compares request
/// variables, so this test fails unless the resolver really asks for archived
/// entities. The archived record is then read back (the details query opts into
/// archived entities) and the unarchive mutation reports the restored name and
/// URL.
#[test]
fn initiative_unarchive_finds_archived_initiative_by_url() {
    const ARCHIVED_ID: &str = "c44f9540-2e02-42c9-bec9-82e4e9529bc0";
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            "ResolveInitiativeBySlug",
            json!({ "data": { "initiatives": { "nodes": [{ "id": ARCHIVED_ID }] } } }),
        )
        .with_variables(json!({ "slugId": "43bc13e544d9", "includeArchived": true })),
        MockResponse::new(
            "GetInitiativeForUnarchive",
            json!({ "data": { "initiatives": { "nodes": [{
                "id": ARCHIVED_ID,
                "slugId": "43bc13e544d9",
                "name": "Archived initiative",
                "archivedAt": "2026-09-20T00:00:00Z"
            }] } } }),
        ),
        MockResponse::new(
            "UnarchiveInitiative",
            json!({ "data": { "initiativeUnarchive": {
                "success": true,
                "entity": {
                    "id": ARCHIVED_ID,
                    "slugId": "43bc13e544d9",
                    "name": "Archived initiative",
                    "url": "https://linear.app/url-test-workspace/initiative/archived-initiative-43bc13e544d9"
                }
            } } }),
        ),
    ]);

    // Upstream sets `LINEAR_WORKSPACE=url-test-workspace` so the URL passes its
    // workspace check; this port has no such variable (it reads the workspace
    // from `--workspace`/config, and `--workspace` cannot be combined with the
    // `LINEAR_API_KEY` the harness supplies), so the plain mock environment is
    // used and the check is a no-op.
    let out = run_cli(
        &[
            "initiative",
            "unarchive",
            "https://linear.app/url-test-workspace/initiative/archived-initiative-43bc13e544d9",
            "--force",
        ],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout
            .contains("Unarchived initiative: Archived initiative"),
        "stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains(
            "https://linear.app/url-test-workspace/initiative/archived-initiative-43bc13e544d9"
        ),
        "stdout: {}",
        out.stdout
    );
}

/// The name branch of the archived-aware resolver, exercised through
/// `initiative delete` (upstream has no by-name-archived case in either
/// command's suite; this is the port's guard for the branch and for delete's
/// own switch to that resolver).
///
/// Upstream's delete resolver declares `GetInitiativeByNameForDelete` with
/// `includeArchived: true` hardcoded in the document; unarchive declares the
/// same body as `GetInitiativeByNameIncludeArchived`. The port keeps one shared
/// document, so the operation name below is the port's. The mock pins it, the
/// `includeArchived: true` text, and the `$name` variable, and the slug probe
/// that runs first is mocked empty so the name step is what must resolve the
/// archived initiative.
#[test]
fn initiative_delete_finds_archived_initiative_by_name() {
    const ARCHIVED_ID: &str = "c44f9540-2e02-42c9-bec9-82e4e9529bc0";
    let server = MockLinearServer::start(vec![
        // Step 3: no slug ID matches the name, so the resolver falls through.
        MockResponse::new(
            "ResolveInitiativeBySlug",
            json!({ "data": { "initiatives": { "nodes": [] } } }),
        )
        .with_variables(json!({ "slugId": "Legacy Initiative", "includeArchived": true })),
        // Step 4: an archived initiative, resolved by exact name.
        MockResponse::new(
            "ResolveInitiativeByNameIncludeArchived",
            json!({ "data": { "initiatives": { "nodes": [{
                "id": ARCHIVED_ID,
                "name": "Legacy Initiative",
                "slugId": "legacy-43bc13e5"
            }] } } }),
        )
        .with_query_includes("includeArchived: true")
        .with_variables(json!({ "name": "Legacy Initiative" })),
        MockResponse::new(
            "GetInitiativeForDelete",
            json!({ "data": { "initiative": {
                "id": ARCHIVED_ID,
                "slugId": "legacy-43bc13e5",
                "name": "Legacy Initiative",
                "projects": { "nodes": [] }
            } } }),
        )
        .with_variables(json!({ "id": ARCHIVED_ID })),
        MockResponse::new(
            "DeleteInitiative",
            json!({ "data": { "initiativeDelete": { "success": true } } }),
        )
        .with_variables(json!({ "id": ARCHIVED_ID })),
    ]);

    let out = run_cli(
        &["initiative", "delete", "Legacy Initiative", "--force"],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout
            .contains("✓ Permanently deleted initiative: Legacy Initiative"),
        "stdout: {}",
        out.stdout
    );
}

/// `initiative-comment.test.ts` — "Initiative Comment Command - Help Through
/// Parent". Parsing through the parent group is the assertion: a missing
/// `comment` registration fails here.
///
/// Upstream snapshots cliffy's layout (`Usage: COMMAND comment`, a two-column
/// grid). The port prints clap's layout instead, so the stable assertions are
/// the group description and the two subcommands with their descriptions.
#[test]
fn initiative_comment_help_through_parent() {
    let server = MockLinearServer::start(vec![]);
    let out = run_cli(
        &["initiative", "comment", "--help"],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("Manage initiative comments"),
        "stdout: {}",
        out.stdout
    );
    assert!(out.stdout.contains("add"), "stdout: {}", out.stdout);
    assert!(out.stdout.contains("list"), "stdout: {}", out.stdout);
    assert!(
        out.stdout.contains(
            "Add a comment or reply to an initiative's discussion (by ID, slug, or name)"
        ),
        "stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains("List comments on an initiative (by ID, slug, or name)"),
        "stdout: {}",
        out.stdout
    );
}

/// `initiative-comment-add.test.ts` — "Initiative Comment Add Command - By UUID
/// With Body Flag". A UUID short-circuits resolution, so `AddComment` is the
/// only request; the port's success text matches upstream's line for line.
#[test]
fn initiative_comment_add_by_uuid_with_body_flag() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "AddComment",
        json!({ "data": { "commentCreate": {
            "success": true,
            "comment": {
                "id": "comment-uuid-1",
                "url": "https://linear.app/team/initiative/platform-abc123/activity#comment-uuid-1"
            }
        } } }),
    )
    .with_variables(json!({
        "input": { "body": "Scope is locked for Q3.", "initiativeId": INITIATIVE_ID }
    }))]);

    let out = run_cli(
        &[
            "initiative",
            "comment",
            "add",
            INITIATIVE_ID,
            "--body",
            "Scope is locked for Q3.",
        ],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout
            .contains(&format!("✓ Comment added to initiative {INITIATIVE_ID}")),
        "stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains("https://linear.app/team/initiative/platform-abc123/activity#comment-uuid-1"),
        "stdout: {}",
        out.stdout
    );
}

/// `initiative-comment-add.test.ts` — "Initiative Comment Add Command - By Name
/// With Reply To Flag". A name goes through the shared resolver (slug first,
/// then case-insensitive name), and a reply still carries `initiativeId`
/// alongside `parentId`.
#[test]
fn initiative_comment_add_by_name_with_reply_to_flag() {
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            "ResolveInitiativeBySlug",
            json!({ "data": { "initiatives": { "nodes": [] } } }),
        )
        .with_variables(json!({ "slugId": "Platform" })),
        MockResponse::new(
            "ResolveInitiativeByName",
            json!({ "data": { "initiatives": { "nodes": [{
                "id": INITIATIVE_ID,
                "name": "Platform",
                "slugId": "platform-abc123"
            }] } } }),
        )
        .with_variables(json!({ "name": "Platform" })),
        MockResponse::new(
            "AddComment",
            json!({ "data": { "commentCreate": {
                "success": true,
                "comment": {
                    "id": "comment-uuid-2",
                    "url": "https://linear.app/team/initiative/platform-abc123/activity#comment-uuid-2"
                }
            } } }),
        )
        .with_variables(json!({
            "input": {
                "body": "Noted.",
                "initiativeId": INITIATIVE_ID,
                "parentId": "comment-uuid-1"
            }
        })),
    ]);

    let out = run_cli(
        &[
            "initiative",
            "comment",
            "add",
            "Platform",
            "--body",
            "Noted.",
            "--reply-to",
            "comment-uuid-1",
        ],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout
            .contains("✓ Comment added to initiative Platform"),
        "stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains("https://linear.app/team/initiative/platform-abc123/activity#comment-uuid-2"),
        "stdout: {}",
        out.stdout
    );
}

/// `initiative-comment-add.test.ts` — "Initiative Comment Add Command - Help".
///
/// Upstream snapshots cliffy's help, including the `withMarkdownHint` paragraph
/// about Linear Markdown and `linear markdown`. The port drops that hint (the
/// convention everywhere in this tree), so only the description, the positional
/// and the three flags — including the `--reply-to` alias — are asserted.
#[test]
fn initiative_comment_add_help() {
    let server = MockLinearServer::start(vec![]);
    let out = run_cli(
        &["initiative", "comment", "add", "--help"],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout
            .contains("Add a comment or reply to an initiative's discussion"),
        "stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("<initiative>"),
        "stdout: {}",
        out.stdout
    );
    assert!(out.stdout.contains("--body"), "stdout: {}", out.stdout);
    assert!(out.stdout.contains("--body-file"), "stdout: {}", out.stdout);
    assert!(out.stdout.contains("--parent"), "stdout: {}", out.stdout);
    assert!(out.stdout.contains("--reply-to"), "stdout: {}", out.stdout);
    assert!(
        out.stdout.contains("Comment body text"),
        "stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains("Read comment body from a file (preferred for markdown content)"),
        "stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains("Reply to a top-level comment by ID (the reply joins that thread)"),
        "stdout: {}",
        out.stdout
    );
}

/// `initiative-comment-list.test.ts` — "Initiative Comment List Command - By
/// UUID". `Initiative` has no comments connection, so the listing goes through
/// the root `comments` query filtered by initiative, sending the UUID as both
/// the entity lookup id and the filter id.
///
/// Upstream's snapshot points the whole thread at `1/15/2024`; the port renders
/// comment dates with `display::format_relative_time` in *local* time, so the
/// date is not asserted — a machine west of UTC-10:30 would print `1/14/2024`.
/// The thread structure, authors, ids and bodies are.
#[test]
fn initiative_comment_list_by_uuid() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "GetInitiativeComments",
        json!({ "data": {
            "initiative": { "id": INITIATIVE_ID, "name": "Platform" },
            "comments": {
                "nodes": [
                    {
                        "id": "comment-uuid-1",
                        "body": "Scope is locked for Q3.",
                        "quotedText": null,
                        "createdAt": "2024-01-15T10:30:00Z",
                        "updatedAt": "2024-01-15T10:30:00Z",
                        "editedAt": null,
                        "url": "https://linear.app/team/initiative/platform-abc123/activity#comment-uuid-1",
                        "user": { "id": "user-uuid-1", "name": "ada", "displayName": "Ada Lovelace" },
                        "externalUser": null,
                        "botActor": null,
                        "parent": null
                    },
                    {
                        "id": "comment-uuid-2",
                        "body": "Noted.",
                        "quotedText": null,
                        "createdAt": "2024-01-15T11:00:00Z",
                        "updatedAt": "2024-01-15T11:00:00Z",
                        "editedAt": null,
                        "url": "https://linear.app/team/initiative/platform-abc123/activity#comment-uuid-2",
                        "user": null,
                        "externalUser": null,
                        "botActor": { "id": "bot-uuid-1", "name": "Slack", "type": "slack", "subType": null },
                        "parent": { "id": "comment-uuid-1" }
                    }
                ],
                "pageInfo": { "hasNextPage": false, "endCursor": "comment-uuid-2" }
            }
        } }),
    )
    // Upstream pins the same filter through `queryIncludes`; its needle has no
    // spaces because its template is minified. This is the port's own text.
    .with_query_includes("filter: { initiative: { id: { eq: $filterId } } }")
    .with_variables(json!({ "id": INITIATIVE_ID, "filterId": INITIATIVE_ID, "after": null }))]);

    let out = run_cli(
        &["initiative", "comment", "list", INITIATIVE_ID],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("@Ada Lovelace commented"),
        "stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("Scope is locked for Q3."),
        "stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("[comment-uuid-1]"),
        "stdout: {}",
        out.stdout
    );
    // The reply is an integration-authored comment: no `user`, so the bot actor
    // name is what renders.
    assert!(
        out.stdout.contains("@Slack replied"),
        "stdout: {}",
        out.stdout
    );
    assert!(out.stdout.contains("Noted."), "stdout: {}", out.stdout);
    assert!(
        out.stdout.contains("[comment-uuid-2]"),
        "stdout: {}",
        out.stdout
    );
}

/// `initiative-comment-list.test.ts` — "Initiative Comment List Command - By
/// Slug JSON Output". A slug goes through the shared resolver first; `--json`
/// re-emits the GraphQL connection shape, `quotedText` included.
#[test]
fn initiative_comment_list_by_slug_json() {
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            "ResolveInitiativeBySlug",
            json!({ "data": { "initiatives": { "nodes": [{ "id": INITIATIVE_ID }] } } }),
        )
        .with_variables(json!({ "slugId": "platform-abc123" })),
        MockResponse::new(
            "GetInitiativeComments",
            json!({ "data": {
                "initiative": { "id": INITIATIVE_ID, "name": "Platform" },
                "comments": {
                    "nodes": [
                        {
                            "id": "comment-uuid-1",
                            "body": "Scope is locked for Q3.",
                            "quotedText": null,
                            "createdAt": "2024-01-15T10:30:00Z",
                            "updatedAt": "2024-01-15T10:30:00Z",
                            "editedAt": null,
                            "url": "https://linear.app/team/initiative/platform-abc123/activity#comment-uuid-1",
                            "user": { "id": "user-uuid-1", "name": "ada", "displayName": "Ada Lovelace" },
                            "externalUser": null,
                            "botActor": null,
                            "parent": null
                        },
                        {
                            "id": "comment-uuid-2",
                            "body": "Noted.",
                            "quotedText": null,
                            "createdAt": "2024-01-15T11:00:00Z",
                            "updatedAt": "2024-01-15T11:00:00Z",
                            "editedAt": null,
                            "url": "https://linear.app/team/initiative/platform-abc123/activity#comment-uuid-2",
                            "user": null,
                            "externalUser": null,
                            "botActor": { "id": "bot-uuid-1", "name": "Slack", "type": "slack", "subType": null },
                            "parent": { "id": "comment-uuid-1" }
                        }
                    ],
                    "pageInfo": { "hasNextPage": false, "endCursor": "comment-uuid-2" }
                }
            } }),
        )
        .with_query_includes("quotedText")
        .with_variables(json!({ "id": INITIATIVE_ID, "filterId": INITIATIVE_ID, "after": null })),
    ]);

    let out = run_cli(
        &["initiative", "comment", "list", "platform-abc123", "--json"],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);

    let parsed: serde_json::Value = serde_json::from_str(&out.stdout).expect("json output");
    assert_eq!(parsed["nodes"][0]["id"], "comment-uuid-1");
    assert_eq!(parsed["nodes"][0]["quotedText"], serde_json::Value::Null);
    assert_eq!(parsed["nodes"][0]["user"]["displayName"], "Ada Lovelace");
    assert_eq!(parsed["nodes"][1]["botActor"]["name"], "Slack");
    assert_eq!(parsed["nodes"][1]["parent"]["id"], "comment-uuid-1");
    assert_eq!(parsed["pageInfo"]["hasNextPage"], false);
    assert_eq!(parsed["pageInfo"]["endCursor"], "comment-uuid-2");
}

/// `initiative-comment-list.test.ts` — "Initiative Comment List Command - No
/// Comments". The port uses upstream's exact notice.
#[test]
fn initiative_comment_list_no_comments() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "GetInitiativeComments",
        json!({ "data": {
            "initiative": { "id": INITIATIVE_ID, "name": "Platform" },
            "comments": { "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } }
        } }),
    )]);

    let out = run_cli(
        &["initiative", "comment", "list", INITIATIVE_ID],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert_eq!(out.stdout.trim(), "No comments found for this initiative");
}

/// The initiative the status tests read back before updating.
fn initiative_for_update(status: &str) -> serde_json::Value {
    json!({ "data": { "initiative": {
        "id": INITIATIVE_ID,
        "slugId": "platform",
        "name": "Platform",
        "description": null,
        "status": status,
        "targetDate": null,
        "color": null,
        "icon": null,
        "owner": null
    } } })
}

fn updated_initiative_response() -> serde_json::Value {
    json!({ "data": { "initiativeUpdate": {
        "success": true,
        "initiative": {
            "id": INITIATIVE_ID,
            "slugId": "platform",
            "name": "Platform",
            "url": "https://linear.app/example/initiative/platform"
        }
    } } })
}

/// `--status` must send the enum spelling the API declares, not a lower-cased
/// one. `InitiativeStatus` is case-sensitive (`Planned | Active | Completed |
/// ...`), so sending `active` — what upstream's `status.toLowerCase()` does for
/// every input — is rejected. Gating the mutation on `status: "Active"` is how
/// the harness pins the payload; the reply refuses to match while the input
/// carries anything else.
#[test]
fn initiative_update_status_is_sent_in_the_enum_case() {
    let server = MockLinearServer::start(vec![
        MockResponse::new("GetInitiativeForUpdate", initiative_for_update("Planned"))
            .with_variables(json!({ "id": INITIATIVE_ID })),
        MockResponse::new("UpdateInitiative", updated_initiative_response())
            .with_variables(json!({ "id": INITIATIVE_ID, "input": { "status": "Active" } })),
    ]);

    let out = run_cli(
        &["initiative", "update", INITIATIVE_ID, "--status", "Active"],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("✓ Updated initiative: Platform"),
        "stdout: {}",
        out.stdout
    );
}

/// A lower-cased `--status` is accepted and canonicalised, rather than sent
/// verbatim as a value the enum rejects — the same spelling a real terminal
/// wizard answers with.
#[test]
fn initiative_update_status_accepts_any_casing() {
    let server = MockLinearServer::start(vec![
        MockResponse::new("GetInitiativeForUpdate", initiative_for_update("Planned"))
            .with_variables(json!({ "id": INITIATIVE_ID })),
        MockResponse::new("UpdateInitiative", updated_initiative_response())
            .with_variables(json!({ "id": INITIATIVE_ID, "input": { "status": "Active" } })),
    ]);

    let out = run_cli(
        &["initiative", "update", INITIATIVE_ID, "--status", "active"],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
}
