//! The docs have to describe the CLI that exists.
//!
//! Prose cannot be compiled, but most of what these documents *do* is name commands and flags, and
//! those can be checked. This walks the real command tree (the same walk `json_coverage.rs` makes)
//! and reads the repository's six documents, then fails when
//!
//! * a literal invocation (`linear issue create --body-file …`, in a fenced block or an inline
//!   span that starts with `linear `) names a command the binary does not answer, or a flag the
//!   command - or a command under it - does not take, or
//! * the command table in `README.md` leaves out a command the binary has.
//!
//! The completeness half is the half that mattered: the table said `cycle list · view` for two
//! commands that had grown `update · archive`, and `label list · create · delete` for a group that
//! had grown `update` - nothing noticed until a person read it, which is not a mechanism.
//!
//! The rule for "resolves" is the binary's own: a mention is fine if running it answers. That is
//! what lets hidden aliases (`issue list` is `issue mine`) and positional arguments
//! (`linear completions bash`) pass without a hand-kept list of exceptions.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use common::run_cli;

const DOCS: &[&str] = &[
    "README.md",
    "AGENTS.md",
    "crates/bridge/README.md",
    "docs/CLI-AUDIT.md",
    "docs/issue-list-commands.md",
    "docs/porting-reference.md",
];

/// The command table lives in the README; the other documents are checked for their invocations.
const TABLE: &str = "README.md";

fn repo(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(path)
}

/// The subcommands a help screen lists.
///
/// A wrapped *description* is indented past the name column, so only a line that starts at exactly
/// the name column is a command - the same rule `json_coverage.rs` learned.
fn subcommands(path: &[&str]) -> Vec<String> {
    let mut args: Vec<&str> = path.to_vec();
    args.push("--help");
    let out = run_cli(&args, &[]).stdout;

    let mut names = Vec::new();
    let mut inside = false;
    for line in out.lines() {
        if line.starts_with("Commands:") {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        if line.trim().is_empty() || line.starts_with("Options:") || line.starts_with("Arguments:")
        {
            break;
        }
        let Some(rest) = line.strip_prefix("  ") else {
            continue;
        };
        if rest.starts_with(' ') {
            continue;
        }
        let name = rest.split_whitespace().next().unwrap_or("");
        if !name.is_empty()
            && name != "help"
            && name.starts_with(|c: char| c.is_ascii_lowercase())
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            names.push(name.to_string());
        }
    }
    names
}

/// Whether the binary answers this command: `--help` exits 0. An alias or a hidden command passes;
/// a name nothing answers does not.
fn resolves(path: &[&str]) -> bool {
    let mut args: Vec<&str> = path.to_vec();
    args.push("--help");
    run_cli(&args, &[]).code == Some(0)
}

fn help_of(path: &[&str]) -> String {
    let mut args: Vec<&str> = path.to_vec();
    args.push("--help");
    run_cli(&args, &[]).stdout
}

/// A command takes a flag if its own help lists it, or if a command under it does - a group's help
/// does not repeat its subcommands' options, and `linear issue comment add --body-file` is a real
/// invocation even though the flag belongs to `comment add`, not to the `issue` group.
fn accepts_flag(path: &[&str], flag: &str) -> bool {
    if help_of(path).contains(flag) {
        return true;
    }
    if path.len() >= 3 {
        return false;
    }
    subcommands(path)
        .iter()
        .filter(|name| name.as_str() != "help")
        .any(|name| {
            let mut deeper: Vec<&str> = path.to_vec();
            deeper.push(name);
            accepts_flag(&deeper, flag)
        })
}

/// The words of a line, with sentence punctuation stripped, so `--json,` compares as `--json`.
fn words(line: &str) -> Vec<String> {
    line.split_whitespace()
        .map(|word| {
            word.trim_matches(|c: char| !c.is_alphanumeric() && c != '-')
                .to_string()
        })
        .filter(|word| !word.is_empty())
        .collect()
}

/// A token that could only be a command name: lowercase, alphanumeric and hyphens.
fn looks_like_a_command(word: &str) -> bool {
    !word.is_empty()
        && word.starts_with(|c: char| c.is_ascii_lowercase())
        && word
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Every leaf command and its own help text - the same walk `json_coverage.rs` makes, because the
/// numbers the README states are counts of *that* tree: the one an agent sees at the command line.
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

/// The integers a line states, in the order it states them.
fn integers(line: &str) -> Vec<usize> {
    line.split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .collect()
}

#[test]
fn the_numbers_the_readme_states_are_this_binarys_numbers() {
    // Only the service shape has the whole tree: without the feature, the README is describing a
    // CLI this test binary is not, and every count would be short by the `sync`/`webhook` groups.
    if !cfg!(feature = "service") {
        return;
    }

    let mut leaves = Vec::new();
    walk(&mut Vec::new(), &mut leaves);
    let groups = subcommands(&[]).len();
    let with_json = leaves
        .iter()
        .filter(|(_, help)| help.contains("--json"))
        .count();

    let text = std::fs::read_to_string(repo(TABLE)).expect("the README");

    // "21 groups, 103 leaf commands." - written as digits precisely so this can read them.
    let counts = text
        .lines()
        .find(|line| line.contains(" groups, ") && line.contains(" leaf commands"))
        .expect("the README should claim a count of its commands");
    assert_eq!(
        integers(counts),
        vec![groups, leaves.len()],
        "the README's command count is stale (this tree: {groups} groups, {} leaves):\n  {counts}",
        leaves.len()
    );

    // "--json is on 45 of the 103 leaf commands, and the other 58 ..." - three numbers, all of
    // them measurements, and the third one has to be the difference of the first two.
    let contract = text
        .lines()
        .find(|line| line.contains("`--json` is on "))
        .expect("the README should say how many commands answer --json");
    assert_eq!(
        integers(contract),
        vec![with_json, leaves.len(), leaves.len() - with_json],
        "the README's --json count is stale (this tree: {with_json} of {}):\n  {contract}",
        leaves.len()
    );
}

/// Every literal invocation in a document: fenced code-block lines, and inline code spans whose
/// first word is `linear`. Prose that merely mentions a command is not an assertion that it runs,
/// so it is not read here - the command table covers the naming.
fn invocations(text: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut fenced = false;
    for (number, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            let stripped = trimmed.trim_start_matches("$ ").trim_start_matches("# ");
            // A trailing shell comment is prose, and these examples are full of them: read as
            // commands, `# resolve the config, print it` turns into `linear config print`.
            let stripped = stripped.split(" #").next().unwrap_or(stripped).trim();
            if stripped.starts_with("linear ") {
                found.push((number + 1, stripped.to_string()));
            }
            continue;
        }
        // Inline spans: only `linear …`, so a sentence that mentions a command inside a long code
        // span of prose is not read as an invocation.
        for (index, span) in line.split('`').enumerate() {
            if index % 2 == 0 {
                continue;
            }
            let span = span.trim();
            if let Some(rest) = span.strip_prefix("linear ") {
                if !rest.contains('`') && !rest.contains("…") && !rest.contains("<") {
                    found.push((number + 1, span.to_string()));
                }
            }
        }
    }
    found
}

/// The commands the `service` feature adds. In a build without it `linear sync` genuinely does not
/// exist, so an invocation naming one is skipped rather than failed here - the gate and CI run
/// `--all-features`, where the same invocations are checked for real.
const SERVICE_ONLY: &[&str] = &["sync", "webhook"];

#[test]
fn every_invocation_the_docs_show_works() {
    let groups: BTreeSet<String> = subcommands(&[]).into_iter().collect();
    assert!(
        groups.len() > 15,
        "the walk saw only {} groups",
        groups.len()
    );
    let service = cfg!(feature = "service");

    let mut problems: Vec<String> = Vec::new();

    for doc in DOCS {
        let text = std::fs::read_to_string(repo(doc))
            .unwrap_or_else(|error| panic!("{doc} should be readable: {error}"));

        for (line, invocation) in invocations(&text) {
            let tokens = words(&invocation);
            // The command is the first token that is a group: global flags may come first
            // (`linear --workspace wave-cloud issue view …`), and `linear --help` has no group at all.
            let Some(start) = tokens.iter().position(|token| groups.contains(token)) else {
                continue;
            };
            if !service {
                if let Some(token) = tokens.get(start) {
                    if SERVICE_ONLY.contains(&token.as_str()) {
                        continue;
                    }
                }
            }
            let group = tokens[start].clone();
            // The longest prefix that resolves is the command; the rest is arguments and flags.
            let mut path = vec![group.as_str()];
            let mut consumed = start + 1;
            if let Some(sub) = tokens.get(start + 1) {
                if looks_like_a_command(sub) {
                    let mut with_sub: Vec<&str> = path.clone();
                    with_sub.push(sub);
                    if resolves(&with_sub) {
                        path = with_sub;
                        consumed = start + 2;
                    } else if SERVICE_ONLY.contains(&group.as_str()) && !service {
                        // cannot be verified in this shape; the gate checks it with the feature on
                    } else {
                        problems.push(format!(
                            "{doc}:{line}: `linear {}` is not a command (try `linear {group} --help`)",
                            path.join(" ")
                        ));
                    }
                }
            }
            for token in tokens.iter().skip(consumed) {
                if !token.starts_with("--") || token.len() < 4 {
                    continue;
                }
                let flag = token.trim_end_matches(',');
                if !accepts_flag(&path, flag) {
                    let direct = path.len() == 1;
                    problems.push(format!(
                        "{doc}:{line}: `linear {}` does not take `{flag}`{}",
                        path.join(" "),
                        if direct {
                            " (nor does any command under it)".to_string()
                        } else {
                            String::new()
                        }
                    ));
                }
            }
        }
    }

    assert!(
        problems.is_empty(),
        "the docs show invocations the CLI does not accept:\n  {}",
        problems.join("\n  ")
    );
}

#[test]
fn the_command_table_lists_every_command() {
    let groups = subcommands(&[]);
    let text = std::fs::read_to_string(repo(TABLE)).expect("the README");

    // group -> the single-word spans its row mentions
    let mut rows: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("| `") else {
            continue;
        };
        let Some((group, cells)) = rest.split_once("` |") else {
            continue;
        };
        if !groups.contains(&group.to_string()) {
            continue;
        }
        let spans = rows.entry(group.to_string()).or_default();
        for (index, cell) in cells.split('`').enumerate() {
            if index % 2 == 1 && looks_like_a_command(cell.trim()) {
                spans.insert(cell.trim().to_string());
            }
        }
    }

    let mut problems: Vec<String> = Vec::new();

    for group in &groups {
        let Some(mentioned) = rows.get(group) else {
            problems.push(format!("{TABLE}: the table has no row for `{group}`"));
            continue;
        };
        for sub in subcommands(&[group.as_str()]) {
            if sub != "help" && !mentioned.contains(&sub) {
                problems.push(format!(
                    "{TABLE}: the `{group}` row does not mention `{sub}`"
                ));
            }
        }
        // Every command-looking name in the row has to be one: a row that names a command the CLI
        // does not have is worse than a row that is short.
        for name in mentioned {
            if name == group {
                continue;
            }
            let mut path = vec![group.as_str()];
            path.push(name.as_str());
            if !resolves(&path) && !subcommands(&[group.as_str()]).contains(name) {
                problems.push(format!(
                    "{TABLE}: the `{group}` row names `{name}`, which is not a `{group}` command"
                ));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "the command table and the CLI disagree:\n  {}",
        problems.join("\n  ")
    );
}
