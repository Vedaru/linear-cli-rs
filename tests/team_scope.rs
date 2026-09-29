//! The configured team is a *reference*, not a key.
//!
//! `linear config` writes `team_id` as a team key, but upstream reads it back
//! through `resolveTeam(...)`. The port used the configured value verbatim as a
//! key in team-scoped filters (upper-casing it first), so any other form - a
//! UUID, a team name, a key from another workspace - silently emptied every
//! team-scoped listing instead of resolving or failing. These tests pin the
//! resolved behaviour by gating the listing response on the request carrying the
//! team's canonical KEY: if the configured value is passed through unresolved,
//! the mock refuses to answer and the command fails loudly.

mod common;

use common::{mock_env, run_cli, MockLinearServer, MockResponse};
use serde_json::json;

const WAVE_TEAM_ID: &str = "35feb448-7bc2-4bcb-a949-a58c7572949a";
const WAVE_TEAM_KEY: &str = "WAV";
const WAVE_PROJECT_SLUG: &str = "wave-progress";

/// The team lookup the configured reference triggers. Answers any reference, so
/// the test is about resolution happening at all, not about which form it had.
fn team_lookup_response() -> MockResponse {
    MockResponse::new(
        "FindTeam",
        json!({ "data": {
            "teams": { "nodes": [{ "id": WAVE_TEAM_ID, "key": WAVE_TEAM_KEY, "name": "WAVE-cloud" }] },
            "teamById": { "nodes": [{ "id": WAVE_TEAM_ID, "key": WAVE_TEAM_KEY, "name": "WAVE-cloud" }] }
        } }),
    )
}

/// The listing, gated on the filter carrying the canonical key.
fn projects_response() -> MockResponse {
    MockResponse::new(
        "GetProjects",
        json!({ "data": { "projects": {
            "nodes": [{
                "id": "id-wave-progress",
                "name": "WAVE项目进度看板",
                "description": null,
                "slugId": WAVE_PROJECT_SLUG,
                "icon": null,
                "color": "#26b5ce",
                "sortOrder": 1.0,
                "status": { "id": "st-backlog", "name": "Backlog", "color": "#F2994A", "type": "backlog" },
                "lead": null,
                "priority": 0,
                "health": null,
                "startDate": null,
                "targetDate": null,
                "startedAt": null,
                "completedAt": null,
                "canceledAt": null,
                "createdAt": "2026-09-29T10:04:05.424Z",
                "updatedAt": "2026-09-29T10:10:00.000Z",
                "url": "https://linear.app/wave-cloud/project/wave-progress",
                "teams": { "nodes": [{ "key": WAVE_TEAM_KEY }] }
            }],
            "pageInfo": { "hasNextPage": false, "endCursor": null }
        } } }),
    )
    .with_variables(json!({
        "filter": { "accessibleTeams": { "some": { "key": { "eq": WAVE_TEAM_KEY } } } }
    }))
}

fn env_with_configured_team(server: &MockLinearServer, reference: &str) -> Vec<(String, String)> {
    let mut env = mock_env(server);
    env.push(("LINEAR_TEAM_ID".to_string(), reference.to_string()));
    env
}

/// A UUID in `team_id` — the field's literal meaning — must resolve to the team,
/// not be compared against team keys.
#[test]
fn configured_team_uuid_resolves_to_the_team_key() {
    let server = MockLinearServer::start(vec![team_lookup_response(), projects_response()]);
    let out = run_cli(
        &["project", "list"],
        &env_with_configured_team(&server, WAVE_TEAM_ID),
    );

    assert!(
        out.success(),
        "a UUID in the config must resolve, not filter everything out: stderr={}",
        out.stderr
    );
    assert!(
        out.stdout.contains(WAVE_PROJECT_SLUG),
        "expected the team's project: {}",
        out.stdout
    );
}

/// A lower-case key keeps working: `find_team` matches keys case-insensitively,
/// so the old unconditional upper-casing was never what made this path work.
#[test]
fn configured_lowercase_team_key_still_resolves() {
    let server = MockLinearServer::start(vec![team_lookup_response(), projects_response()]);
    let out = run_cli(
        &["project", "list"],
        &env_with_configured_team(&server, "wav"),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains(WAVE_PROJECT_SLUG),
        "stdout: {}",
        out.stdout
    );
}

/// A team that does not exist must be an error. Before the fix this was the
/// dangerous case: the value went into the filter untouched, Linear matched
/// nothing, and the command reported success with an empty result.
#[test]
fn unknown_configured_team_is_an_error_not_an_empty_listing() {
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            "FindTeam",
            json!({ "data": {
                "teams": { "nodes": [] },
                "teamById": { "nodes": [] }
            } }),
        ),
        MockResponse::new(
            "GetAllTeams",
            json!({ "data": { "teams": { "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } } } }),
        ),
    ]);
    let out = run_cli(
        &["project", "list"],
        &env_with_configured_team(&server, "NOSUCHTEAM"),
    );

    assert!(
        !out.success(),
        "an unresolvable configured team must fail, not print an empty listing: stdout={}",
        out.stdout
    );
    assert!(
        out.stderr.contains("NOSUCHTEAM"),
        "the error should name the reference: {}",
        out.stderr
    );
}
