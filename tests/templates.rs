//! `linear template` — local templates, the unified lookup, and `issue create --template`.
//!
//! The local half needs no API at all, which is what the scratch `HOME` in these tests asserts: a
//! template is a file, and reading one must not require Linear. The two API-backed tests are the
//! ones that matter for the ticket's "done when" — a local template *and* a workspace template of
//! the same name both work, and the shadowed one is named rather than silently losing.

mod common;

use common::{run_cli, MockLinearServer, MockResponse};
use serde_json::{json, Value};

/// A scratch config directory, so a test never reads or writes the developer's own templates.
fn scratch(name: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("temp dir");
    let config = dir.path().join(".config");
    let path = config
        .join("linear")
        .join("templates")
        .join(format!("{name}.toml"));
    (dir, path.to_string_lossy().to_string())
}

fn env_for(server: Option<&MockLinearServer>, home: &tempfile::TempDir) -> Vec<(String, String)> {
    let mut env = match server {
        Some(server) => common::mock_env(server),
        None => Vec::new(),
    };
    env.push((
        "HOME".to_string(),
        home.path().to_string_lossy().to_string(),
    ));
    env.push((
        "XDG_CONFIG_HOME".to_string(),
        home.path().join(".config").to_string_lossy().to_string(),
    ));
    env
}

#[test]
fn a_local_template_is_a_file_and_reads_back_without_an_api_key() {
    let (home, path) = scratch("bug");
    let env = env_for(None, &home);

    let created = run_cli(
        &[
            "template",
            "create",
            "bug",
            "--title",
            "Bug: ",
            "--label",
            "Bug",
            "--priority",
            "2",
        ],
        &env,
    );
    assert!(created.success(), "stderr: {}", created.stderr);
    let written = std::fs::read_to_string(&path).expect("the template file");
    assert!(written.contains("title = \"Bug: \""), "{written}");

    let shown = run_cli(&["template", "show", "bug", "--json"], &env);
    assert!(shown.success(), "stderr: {}", shown.stderr);
    let parsed: Value = serde_json::from_str(&shown.stdout).expect("show --json is JSON");
    assert_eq!(parsed["kind"], json!("local"));
    assert_eq!(parsed["fields"]["title"], json!("Bug: "));
    assert_eq!(parsed["fields"]["priority"], json!(2));
    assert_eq!(parsed["fields"]["labels"][0], json!("Bug"));
}

#[test]
fn a_template_name_that_could_escape_the_directory_is_refused() {
    let (home, _path) = scratch("unused");
    let env = env_for(None, &home);

    for name in ["../escape", "a/b", ".hidden"] {
        let out = run_cli(&["template", "create", name, "--title", "x"], &env);
        assert!(!out.success(), "{name} should be refused");
        assert!(
            out.stderr.contains("not a usable template name"),
            "stderr: {}",
            out.stderr
        );
    }
}

#[test]
fn template_update_replaces_only_the_fields_it_is_given() {
    let (home, path) = scratch("bug");
    let env = env_for(None, &home);

    let created = run_cli(
        &[
            "template",
            "create",
            "bug",
            "--title",
            "Bug: ",
            "--priority",
            "2",
        ],
        &env,
    );
    assert!(created.success(), "stderr: {}", created.stderr);

    let updated = run_cli(
        &["template", "update", "bug", "--title", "Bug report: "],
        &env,
    );
    assert!(updated.success(), "stderr: {}", updated.stderr);

    let written = std::fs::read_to_string(&path).expect("the template file");
    assert!(written.contains("Bug report: "), "{written}");
    assert!(
        written.contains("priority = 2"),
        "the priority is left alone: {written}"
    );
}

#[test]
fn template_delete_needs_force_off_a_terminal_and_removes_the_file() {
    let (home, path) = scratch("bug");
    let env = env_for(None, &home);
    assert!(run_cli(&["template", "create", "bug", "--title", "Bug: "], &env).success());

    let refused = run_cli(&["template", "delete", "bug"], &env);
    assert!(!refused.success());
    assert!(
        refused.stderr.contains("Interactive confirmation required"),
        "stderr: {}",
        refused.stderr
    );
    assert!(std::path::Path::new(&path).exists());

    let deleted = run_cli(&["template", "delete", "bug", "--force"], &env);
    assert!(deleted.success(), "stderr: {}", deleted.stderr);
    assert!(
        !std::path::Path::new(&path).exists(),
        "the file should be gone"
    );
}

fn find_team_mock() -> MockResponse {
    MockResponse::new(
        "FindTeam",
        json!({ "data": {
            "teams": { "nodes": [{
                "id": common::ENG_TEAM_ID, "key": common::ENG_TEAM_KEY, "name": common::ENG_TEAM_NAME
            }] },
            "teamById": { "nodes": [] }
        } }),
    )
}

fn templates_mock(names: &[&str]) -> MockResponse {
    let nodes: Vec<Value> = names
        .iter()
        .map(|name| {
            json!({
                "id": format!("template-{name}"),
                "name": name,
                "description": null,
                "type": "issue",
                "icon": null,
                "color": null,
                "hasFormFields": false,
                "lastAppliedAt": null,
                "sortOrder": 0.0,
                "createdAt": "2026-09-01T00:00:00.000Z",
                "updatedAt": "2026-09-01T00:00:00.000Z",
                "team": { "id": common::ENG_TEAM_ID, "key": common::ENG_TEAM_KEY, "name": common::ENG_TEAM_NAME },
                "inheritedFrom": null,
                "creator": null,
                "templateData": "{}"
            })
        })
        .collect();
    // `templates` is a *list* in Linear's schema, not a connection: the CLI reads it as an array.
    MockResponse::new("GetTemplates", json!({ "data": { "templates": nodes } }))
}

/// The ticket's first half: a local template drives `issue create`.
#[test]
fn issue_create_applies_a_local_template() {
    let (home, _path) = scratch("bug");
    let server = MockLinearServer::start(vec![
        find_team_mock(),
        templates_mock(&[]),
        MockResponse::new(
            "CreateIssue",
            json!({ "data": { "issueCreate": { "success": true, "issue": {
                "id": "issue-9",
                "identifier": "ENG-9",
                "title": "Bug: ",
                "url": "https://linear.app/example/issue/ENG-9",
                "useDefaultTemplate": false
            } } } }),
        )
        // The gate is the assertion: the template's title travels, the default template is
        // suppressed, and nothing else is filled in that the caller did not ask for.
        .with_variables(json!({ "input": {
            "title": "Bug: ",
            "labelIds": [],
            "teamId": common::ENG_TEAM_ID,
            "useDefaultTemplate": false
        } })),
    ]);
    let env = env_for(Some(&server), &home);

    let created = run_cli(&["template", "create", "bug", "--title", "Bug: "], &env);
    assert!(created.success(), "stderr: {}", created.stderr);

    let out = run_cli(
        &[
            "issue",
            "create",
            "--team",
            common::ENG_TEAM_KEY,
            "--template",
            "bug",
        ],
        &env,
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("Using local template bug: title"),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains("https://linear.app/example/issue/ENG-9"),
        "{}",
        out.stdout
    );
}

/// The ticket's second half: the workspace template of the same name still exists, and the local
/// file is named as the one that shadowed it.
#[test]
fn seeing_a_local_template_names_the_workspace_template_it_shadows() {
    let (home, _path) = scratch("bug");
    let server = MockLinearServer::start(vec![templates_mock(&["bug"])]);
    let env = env_for(Some(&server), &home);

    let created = run_cli(&["template", "create", "bug", "--title", "Bug: "], &env);
    assert!(created.success(), "stderr: {}", created.stderr);

    let shown = run_cli(&["template", "show", "bug"], &env);

    assert!(shown.success(), "stderr: {}", shown.stderr);
    assert!(
        shown.stdout.contains("bug (local template)"),
        "{}",
        shown.stdout
    );
    // The shadowed name is part of what `show` reports (`shadowedWorkspaceTemplate` in `--json`),
    // not a warning about a failure.
    assert!(
        shown
            .stdout
            .contains("workspace template named \"bug\" also exists and is shadowed"),
        "{}",
        shown.stdout
    );
}
