//! `linear roadmap` - the read-only group, and the decision that keeps it that way.
//!
//! The writes are absent because the API refuses them ("Roadmaps are deprecated, use initiatives
//! instead" - measured before the module was written), so the last test here is the one that
//! matters most: it fails if a `roadmap create` ever appears without someone reading the reason
//! first. The rest pin the shapes, and the projects coming from the roadmap's own relation rather
//! than from a client-side filter of every project in the workspace.

mod common;

use common::{mock_env, run_cli, MockLinearServer, MockResponse};
use serde_json::{json, Value};

fn roadmap(id: &str, name: &str) -> Value {
    json!({
        "id": id,
        "name": name,
        "description": "The plan",
        "color": "#5E6AD2",
        "slugId": "the-plan",
        "archivedAt": null,
        "createdAt": "2026-09-01T00:00:00.000Z",
        "updatedAt": "2026-10-03T00:00:00.000Z",
        "owner": { "id": "user-1", "displayName": "loner" },
        "creator": { "id": "user-1", "displayName": "loner" }
    })
}

fn list_mock(nodes: Vec<Value>) -> MockResponse {
    MockResponse::new(
        "ListRoadmaps",
        json!({ "data": { "roadmaps": {
            "nodes": nodes,
            "pageInfo": { "hasNextPage": false, "endCursor": null }
        } } }),
    )
}

fn get_mock() -> MockResponse {
    MockResponse::new(
        "GetRoadmap",
        json!({ "data": { "roadmap": roadmap("roadmap-1", "The Plan") } }),
    )
}

fn projects_mock(id: &str, nodes: Vec<Value>) -> MockResponse {
    MockResponse::new(
        "RoadmapProjects",
        json!({ "data": { "roadmap": { "projects": {
            "nodes": nodes,
            "pageInfo": { "hasNextPage": false, "endCursor": null }
        } } } }),
    )
    .with_variables(json!({ "id": id }))
}

#[test]
fn list_pins_the_json_shape() {
    let server = MockLinearServer::start(vec![list_mock(vec![roadmap("roadmap-1", "The Plan")])]);

    let output = run_cli(&["roadmap", "list", "--json"], &mock_env(&server));

    assert!(output.success(), "stderr: {}", output.stderr);
    let printed: Value = serde_json::from_str(&output.stdout).expect("a roadmaps document");
    assert_eq!(printed["nodes"][0]["name"], "The Plan");
    assert_eq!(printed["nodes"][0]["owner"]["displayName"], "loner");
    assert_eq!(printed["pageInfo"]["hasNextPage"], false);
}

#[test]
fn an_empty_list_says_why_it_is_empty() {
    // Nothing in this workspace, and nothing that can be made: the human path has to carry the
    // reason, or the reader goes looking for a `roadmap create` that will never exist.
    let server = MockLinearServer::start(vec![list_mock(vec![])]);

    let output = run_cli(&["roadmap", "list"], &mock_env(&server));

    assert!(output.success(), "stderr: {}", output.stderr);
    assert!(
        output.stdout.contains("deprecated"),
        "stdout: {}",
        output.stdout
    );
}

#[test]
fn view_reads_the_projects_from_the_roadmap_relation() {
    // The ticket's done-when: `roadmap view --json` names the projects attached to it. The second
    // request is a claim of its own - the projects come from `roadmap.projects`, not from listing
    // every project in the workspace and filtering.
    let server = MockLinearServer::start(vec![
        get_mock(),
        projects_mock(
            "roadmap-1",
            vec![
                json!({ "id": "project-1", "name": "Linear to Forgejo project mirror", "state": "started" }),
                json!({ "id": "project-2", "name": "linear-cli-rs", "state": "planned" }),
            ],
        ),
    ]);

    let output = run_cli(
        &[
            "roadmap",
            "view",
            "2f1a2b3c-4d5e-6f70-8192-a3b4c5d6e7f8",
            "--json",
        ],
        &mock_env(&server),
    );

    assert!(output.success(), "stderr: {}", output.stderr);
    let printed: Value = serde_json::from_str(&output.stdout).expect("a roadmap");
    let names: Vec<&str> = printed["projects"]["nodes"]
        .as_array()
        .expect("projects")
        .iter()
        .filter_map(|project| project["name"].as_str())
        .collect();
    assert_eq!(
        names,
        vec!["Linear to Forgejo project mirror", "linear-cli-rs"]
    );
}

#[test]
fn a_name_is_matched_against_the_listing() {
    // `roadmaps` takes no filter argument - verified against the live API - so a name is matched
    // locally. The listing is the only request here: the matched node already carries the fields
    // `view` prints, and its projects are read by id afterwards.
    let server = MockLinearServer::start(vec![
        list_mock(vec![
            roadmap("roadmap-1", "Something else"),
            roadmap("roadmap-2", "The Plan"),
        ]),
        projects_mock("roadmap-2", vec![]),
    ]);

    let output = run_cli(
        &["roadmap", "view", "The Plan", "--json"],
        &mock_env(&server),
    );

    assert!(output.success(), "stderr: {}", output.stderr);
    let printed: Value = serde_json::from_str(&output.stdout).expect("a roadmap");
    assert_eq!(printed["id"], "roadmap-2");
}

#[test]
fn a_name_two_roadmaps_share_is_refused_rather_than_guessed() {
    let server = MockLinearServer::start(vec![list_mock(vec![
        roadmap("roadmap-1", "The Plan"),
        roadmap("roadmap-2", "The Plan"),
    ])]);

    let output = run_cli(&["roadmap", "view", "The Plan"], &mock_env(&server));

    assert!(!output.success());
    assert!(
        output.stderr.contains("More than one roadmap"),
        "stderr: {}",
        output.stderr
    );
}

#[test]
fn a_workspace_with_no_roadmaps_points_at_the_successor() {
    let server = MockLinearServer::start(vec![list_mock(vec![])]);

    let output = run_cli(&["roadmap", "view", "The Plan"], &mock_env(&server));

    assert!(!output.success());
    assert!(
        output.stderr.contains("initiative"),
        "the suggestion should name the successor: {}",
        output.stderr
    );
}

#[test]
fn the_write_half_is_absent_and_this_is_where_that_gets_questioned() {
    // Linear deprecated roadmaps and the API refuses `roadmapCreate` by name, so a `roadmap create`
    // here could only fail. If someone adds one anyway, this test is the place the question gets
    // asked: the group is a read surface, and the successor's write half lives under `initiative`.
    let help = run_cli(&["roadmap", "--help"], &[]);
    assert!(help.success(), "stderr: {}", help.stderr);
    assert!(help.stdout.contains("list") && help.stdout.contains("view"));

    for absent in ["create", "update", "delete", "archive", "add-project"] {
        let attempt = run_cli(&["roadmap", absent, "x"], &[]);
        assert!(
            !attempt.success(),
            "`roadmap {absent}` answered - the writes are refused by the API, so this command can \
             only fail; read the reason in src/linear/roadmaps.rs before adding it"
        );
    }
}
