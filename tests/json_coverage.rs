//! Every subcommand answers `--json`, or is listed below with the reason it does not.
//!
//! The premise of this port is that an agent drives it without screen-scraping, so a command with
//! no machine-readable form breaks that quietly: the caller gets a human table and parses it. The
//! tree is walked as the *binary* presents it - not as the clap definitions describe it - because
//! the question is what an agent sees at the command line.
//!
//! [`EXEMPT`] is a ratchet: it may shrink, never grow. The test fails when
//!
//! * a command offers no `--json` and is not listed (a new gap),
//! * a command that now offers `--json` is still listed (the work landed and the entry did not), or
//! * a listed command no longer exists (a stale entry).
//!
//! A **mutation** answers with the API's own payload for the same reason a read does
//! (`{"issueCreate": {"issue": {...}}}`, not a shape of ours to keep in step): the caller can act on
//! the identifier without a second query, and the field names are the ones the server already uses.
//! Where the helper a command calls keeps only an identifier, the document names that identifier and
//! the entry below says so.
//!
//! Gated on `service` like the other suites that walk the whole tree: without the feature the
//! `sync` and `webhook` groups do not exist, and the comparison would be against a different CLI.

#![cfg(feature = "service")]

mod common;

use common::run_cli;

/// Commands without `--json`, and why. Shrink this list; never grow it.
///
/// Two kinds of entry: a command that will *never* carry the flag because its output is not data
/// (a credential, a reference document, a scaffold, the raw API response, or a server), and a
/// command that does not carry it *yet* - a mutation, where success is the exit code today and a
/// machine-readable result is the remaining work on VED-60, or a read that is simply still to do.
const EXEMPT: &[(&str, &str)] = &[
    // --- never: the output is not data ---
    (
        "api",
        "the raw GraphQL passthrough; stdout is already the response document",
    ),
    ("auth login", "prompts for and validates a credential"),
    (
        "auth logout",
        "changes credential state; acknowledgement only",
    ),
    (
        "auth migrate",
        "changes credential state; acknowledgement only",
    ),
    (
        "auth token",
        "prints a credential, which nothing should be encouraged to parse",
    ),
    ("completions", "prints a shell script"),
    ("config service", "prints a configuration scaffold"),
    ("markdown", "prints a reference document"),
    (
        "webhook serve",
        "a long-running server, not a command that returns a result",
    ),
    // --- mutations: success is the exit code today ---
    ("auth default", "sets the default workspace"),
    ("cycle archive", "archives a cycle"),
    ("cycle update", "updates a cycle"),
    ("document comment add", "adds a comment"),
    ("document create", "creates a document"),
    ("document delete", "deletes a document"),
    ("document update", "updates a document"),
    ("initiative add-project", "attaches a project"),
    ("initiative archive", "archives an initiative"),
    ("initiative comment add", "adds a comment"),
    ("initiative create", "creates an initiative"),
    ("initiative delete", "deletes an initiative"),
    ("initiative remove-project", "detaches a project"),
    ("initiative unarchive", "unarchives an initiative"),
    ("initiative update", "updates an initiative"),
    ("initiative-update create", "posts a status update"),
    ("issue archive", "archives an issue"),
    ("issue attach", "creates a link attachment"),
    ("issue comment add", "adds a comment"),
    ("issue comment delete", "deletes a comment"),
    ("issue comment resolve", "resolves a comment thread"),
    ("issue comment unresolve", "unresolves a comment thread"),
    ("issue comment update", "edits a comment"),
    ("issue delete", "deletes an issue"),
    ("issue link", "creates a link"),
    ("issue pull-request", "creates a pull request"),
    ("issue relation add", "creates a relation"),
    ("issue relation delete", "removes a relation"),
    ("issue start", "assigns the issue and moves it to started"),
    ("issue subscribe", "subscribes the viewer"),
    ("issue unarchive", "restores an issue"),
    ("issue unsubscribe", "unsubscribes the viewer"),
    ("label create", "creates a label"),
    ("label delete", "deletes a label"),
    ("label update", "updates a label"),
    ("milestone create", "creates a milestone"),
    ("milestone delete", "deletes a milestone"),
    ("milestone update", "updates a milestone"),
    ("project comment add", "adds a comment"),
    ("project delete", "deletes a project"),
    ("project update", "updates a project"),
    ("project-update create", "posts a status update"),
    ("team create", "creates a team"),
    ("team delete", "deletes a team"),
    // --- never: the payload is another program's to print ---
    (
        "issue commits",
        "delegates to `jj log` with an inherited terminal, so the payload is jj's",
    ),
    (
        "team autolinks",
        "configures GitHub autolinks through `gh`; that command's output is the result",
    ),
];

/// The subcommands a help screen lists, in order.
///
/// Parsed from the text rather than read from clap because the text is what a caller sees; a
/// wrapped description line is rejected by the name check, which is what keeps the parser honest.
fn subcommands(path: &[&str]) -> Vec<String> {
    let mut args = path.to_vec();
    args.push("--help");
    let out = run_cli(&args, &[]);

    let mut names = Vec::new();
    let mut inside = false;
    for line in out.stdout.lines() {
        let line = line.trim_end();
        if line.starts_with("Commands:") {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        if line.is_empty() || line.starts_with("Options:") || line.starts_with("Arguments:") {
            break;
        }
        let Some(rest) = line.strip_prefix("  ") else {
            continue;
        };
        // A wrapped *description* is indented to the description column (three or more spaces), so
        // only a line that starts at exactly the name column is a command. Without this the
        // continuation of a long description reads as a command called `equivalent`.
        if rest.starts_with(' ') {
            continue;
        }
        let name = rest.split_whitespace().next().unwrap_or("");
        let is_a_command_name = !name.is_empty()
            && name != "help"
            && name.starts_with(|c: char| c.is_ascii_lowercase())
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if is_a_command_name {
            names.push(name.to_string());
        }
    }
    names
}

/// Every leaf command and the text of its own help screen.
fn walk(path: &mut Vec<String>, leaves: &mut Vec<(String, String)>) {
    let refs: Vec<&str> = path.iter().map(String::as_str).collect();
    let subs = subcommands(&refs);
    if subs.is_empty() || path.len() >= 3 {
        if !path.is_empty() {
            let mut args: Vec<&str> = path.iter().map(String::as_str).collect();
            args.push("--help");
            leaves.push((path.join(" "), run_cli(&args, &[]).stdout));
        }
        return;
    }
    for sub in subs {
        path.push(sub);
        walk(path, leaves);
        path.pop();
    }
}

#[test]
fn every_subcommand_answers_json_or_says_why_not() {
    let mut leaves = Vec::new();
    walk(&mut Vec::new(), &mut leaves);
    assert!(
        leaves.len() > 80,
        "the walk found only {} commands, so the help parser is wrong",
        leaves.len()
    );

    let mut unflagged = Vec::new();
    let mut surplus = Vec::new();
    let mut stale = Vec::new();

    for (name, help) in &leaves {
        let listed = EXEMPT.iter().any(|(command, _)| command == name);
        match (help.contains("--json"), listed) {
            (false, false) => unflagged.push(name.clone()),
            (true, true) => surplus.push(name.clone()),
            _ => {}
        }
    }
    for (command, _) in EXEMPT {
        if !leaves.iter().any(|(name, _)| name == command) {
            stale.push((*command).to_string());
        }
    }

    assert!(
        unflagged.is_empty(),
        "these commands have no `--json` and are not listed as exempt:\n  {}\n\
         Either give the command a machine-readable form, or add it to EXEMPT with the reason.",
        unflagged.join("\n  ")
    );
    assert!(
        surplus.is_empty(),
        "these commands now answer `--json` but are still listed as exempt - remove them, so the \
         list keeps meaning something:\n  {}",
        surplus.join("\n  ")
    );
    assert!(
        stale.is_empty(),
        "these commands are listed as exempt but no longer exist - remove them:\n  {}",
        stale.join("\n  ")
    );
}
