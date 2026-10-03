//! The premise of this port, measured: a prompt written against upstream keeps working.
//!
//! `tests/fixtures/upstream-surface.txt` pins upstream's command tree and the long flags of each
//! command, rendered from a checkout of `schpet/linear-cli` at a recorded commit (the file's header
//! carries the recipe). This test walks *our* tree - the same walk `json_coverage.rs` and
//! `docs_coverage.rs` make, because the question is what an agent sees at the command line - and
//! fails by name when
//!
//! * an upstream command cannot be reached in our CLI, or
//! * an upstream flag is not accepted by the command that has it upstream (or by any command under
//!   it, since a group's help does not repeat its subcommands' options).
//!
//! Gaps are allowed only by being listed below with a reason, the way `json_coverage.rs` lists the
//! commands that will never answer `--json`: the list may shrink, never grow, so *new* drift is a
//! failing gate rather than a quiet one. Anything we add beyond upstream is not a parity failure at
//! all - this is a floor, not a ceiling, and the additions are documented in `AGENTS.md`.

mod common;

use std::collections::BTreeSet;

use common::run_cli;

/// Upstream flags our CLI does not accept, with the reason and where it is tracked.
///
/// All three are the same case, and it is worth spelling out because the first reading of the
/// fixture got it backwards: upstream *removed* these and kept the declarations as tombstones.
/// `issue-mine.ts` documents them as "Removed: use `issue query --assignee` instead" and raises a
/// validation error naming `linear issue query` when one is passed. So this is not a gap in our
/// port - it is a place where upstream's declaration outlives its behaviour, and ours does not have
/// the dead flags to declare. Nothing to do; the entry exists so the guard can tell this case from
/// a real one.
const UNACCEPTED_FLAGS: &[(&str, &str, &str)] = &[
    (
        "issue mine",
        "--assignee",
        "removed upstream ('use `issue query --assignee` instead'); the declaration is a tombstone",
    ),
    (
        "issue mine",
        "--all-assignees",
        "removed upstream ('use `issue query --all-assignees` instead') - same tombstone",
    ),
    (
        "issue mine",
        "--unassigned",
        "removed upstream; `issue query --unassigned` is the supported spelling, and ours has it",
    ),
];

fn fixture() -> Vec<String> {
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/upstream-surface.txt"),
    )
    .expect("the pinned upstream surface");

    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// The subcommands a help screen lists, at exactly the name column - a wrapped description is
/// indented past it, the same rule `json_coverage.rs` uses.
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

fn help_of(path: &[&str]) -> String {
    let mut args: Vec<&str> = path.to_vec();
    args.push("--help");
    run_cli(&args, &[]).stdout
}

/// Whether the binary answers this command at all - an alias or a hidden command passes, and a
/// name nothing answers does not.
fn resolves(path: &[&str]) -> bool {
    let mut args: Vec<&str> = path.to_vec();
    args.push("--help");
    run_cli(&args, &[]).code == Some(0)
}

/// Whether the command, or any command under it, accepts this flag.
///
/// A group's help does not repeat its subcommands' options, and upstream declares plenty of flags
/// on the subcommand rather than the group - so the question a prompt asks (`does `issue mine`
/// take --assignee?`) is answered by the whole branch.
fn accepts_flag(path: &[&str], flag: &str) -> bool {
    let mut seen: BTreeSet<Vec<String>> = BTreeSet::new();
    let mut frontier: Vec<Vec<String>> = vec![path.iter().map(|s| s.to_string()).collect()];

    while let Some(current) = frontier.pop() {
        if current.len() > 3 || !seen.insert(current.clone()) {
            continue;
        }
        let refs: Vec<&str> = current.iter().map(String::as_str).collect();
        if help_of(&refs).contains(flag) {
            return true;
        }
        for sub in subcommands(&refs) {
            let mut deeper = current.clone();
            deeper.push(sub);
            frontier.push(deeper);
        }
    }
    false
}

#[test]
fn every_upstream_command_is_reachable_here() {
    let mut missing: Vec<String> = Vec::new();

    for line in fixture() {
        if line.contains(" --") {
            continue;
        }
        let path: Vec<&str> = line.split_whitespace().collect();
        if !resolves(&path) {
            missing.push(line);
        }
    }

    assert!(
        missing.is_empty(),
        "upstream has these commands, and this CLI does not answer them:\n  {}\n\
         A command that is deliberately not ported belongs in AGENTS.md and in this test's \
         exception list - the point is that the loss is a decision someone made.",
        missing.join("\n  ")
    );
}

#[test]
fn every_upstream_flag_is_accepted_or_listed_with_a_reason() {
    let mut missing: Vec<String> = Vec::new();

    for line in fixture() {
        let Some((command, flag)) = line.split_once(" --") else {
            continue;
        };
        let path: Vec<&str> = command.split_whitespace().collect();
        let flag = format!("--{flag}");

        // A flag on a command we do not have at all is the other test's failure, not this one.
        if !resolves(&path) || accepts_flag(&path, &flag) {
            continue;
        }
        let excused = UNACCEPTED_FLAGS
            .iter()
            .any(|(excused_command, excused_flag, _)| {
                *excused_command == command && *excused_flag == flag
            });
        if !excused {
            missing.push(line);
        }
    }

    assert!(
        missing.is_empty(),
        "upstream accepts these flags where this CLI does not:\n  {}\n\
         Renaming or dropping a flag breaks prompts written against upstream - that is the premise \
         this port is built on. Add it, or add the pair to UNACCEPTED_FLAGS with the reason.",
        missing.join("\n  ")
    );
}

#[test]
fn the_exception_list_does_not_outlive_the_gaps_it_describes() {
    // The list may only shrink: an entry that is now accepted, or whose command no longer exists,
    // is stale, and a stale entry is how a ratchet stops meaning anything.
    let mut stale: Vec<String> = Vec::new();

    for (command, flag, _) in UNACCEPTED_FLAGS {
        let path: Vec<&str> = command.split_whitespace().collect();
        if !resolves(&path) {
            stale.push(format!("{command} {flag}: the command is gone"));
        } else if accepts_flag(&path, flag) {
            stale.push(format!("{command} {flag}: it is accepted now"));
        }
    }

    assert!(
        stale.is_empty(),
        "these entries are no longer gaps and should be removed:\n  {}",
        stale.join("\n  ")
    );
}
