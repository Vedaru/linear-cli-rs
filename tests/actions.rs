//! `--web`/`-a` and the workspace slug (VED-299).
//!
//! In a config with only `api_key`, the slug is not configured, so the browser
//! helpers ask Linear for `organization.urlKey` rather than refusing. `PATH` is
//! narrowed to an empty directory so `xdg-open` cannot exist and no browser is
//! launched; the "Opening …" line is printed before the opener is attempted.

mod common;

use std::process::Command;

use common::{mock_env, MockLinearServer, MockResponse};
use serde_json::json;

fn run_web(args: &[&str], server: &MockLinearServer) -> (Option<i32>, String) {
    let empty = tempfile::tempdir().expect("temp dir");
    let mut command = Command::new(env!("CARGO_BIN_EXE_linear"));
    command.args(args).env("NO_COLOR", "1").env("PATH", empty.path());
    for (key, value) in mock_env(server) {
        command.env(key, value);
    }
    let output = command.output().expect("linear runs");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.code(), combined)
}

#[test]
fn issue_view_web_derives_the_workspace_slug_from_the_api() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "WorkspaceUrlKey",
        json!({ "data": { "viewer": { "organization": { "urlKey": "wave-cloud" } } } }),
    )]);

    let (_, combined) = run_web(&["issue", "view", "ENG-1", "--web"], &server);

    assert!(
        combined.contains("Opening https://linear.app/wave-cloud/issue/ENG-1 in web browser"),
        "the slug must come from the API: {combined}"
    );
}

#[test]
fn no_slug_anywhere_names_the_workspace_flag() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "WorkspaceUrlKey",
        json!({ "data": { "viewer": { "organization": { "urlKey": null } } } }),
    )]);

    let (code, combined) = run_web(&["issue", "view", "ENG-1", "--web"], &server);

    assert_eq!(code, Some(1), "{combined}");
    assert!(combined.contains("workspace is not set"), "{combined}");
    assert!(
        combined.contains("--workspace"),
        "and it must name the flag to pass: {combined}"
    );
}
