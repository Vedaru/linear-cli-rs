//! One operation, one definition.
//!
//! A GraphQL document's name is its identity. The document checker (`check_documents.py`) validates
//! every embedded document against the live schema, but it validates each *document* - it does not
//! notice two files that define the same operation name, and two copies of a name can drift into two
//! **valid but different** queries. That is not hypothetical: `$id: String` where the schema wanted
//! `String!` passed every test in this repository and failed on the first real call, and the pairs
//! below had already diverged before anyone looked (`GetTeamMembers` exists as a five-field query in
//! `linear/queries.rs` and a seventeen-field one in `team/team_members.rs`, both named the same).
//!
//! So this is a ratchet, in the shape the upstream parity guard uses: the names known to be defined
//! twice are **listed with the reason**, and the list may only shrink. Adding a second definition of
//! an existing name fails immediately, which is the case that matters - a new duplicate is a
//! decision, an existing one is a debt with a ticket.
//!
//! The list is not a licence. Each entry names the issue that removes it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Operation names defined in more than one file, with why, and what removes it.
///
/// Shrinking this list is the work; growing it fails the test.
const KNOWN_DUPLICATES: &[(&str, &str)] = &[
    (
        "GetTeamMembers",
        "DEFECT: the copies differ - five fields from $teamId in linear/queries.rs, seventeen from \
         $teamKey with includeDisabled in commands/team/team_members.rs. Two valid documents, one \
         name, so a fixture that matches by operation name can answer the wrong one. VED-111.",
    ),
    (
        "GetOrganizationMembers",
        "DEFECT: same shape as GetTeamMembers - the shared copy is a subset of the command's. VED-111.",
    ),
    (
        "GetTeamCycles",
        "DEFECT: the copies differ - the shared one asks for the team's key, cyclesEnabled and \
         activeCycle with a hard-coded first: 250, the command's asks for $first and per-cycle \
         endsAt/completedAt/isActive/isFuture/isPast. VED-111.",
    ),
];

/// Every `r#"..."#` literal in a file, which is how this codebase embeds a GraphQL document.
fn raw_strings(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("r#\"") {
        let after = &rest[start + 3..];
        match after.find("\"#") {
            Some(end) => {
                found.push(&after[..end]);
                rest = &after[end + 2..];
            }
            None => break,
        }
    }
    found
}

/// The operation names a chunk of source defines: `query Name(` and `mutation Name(` only.
///
/// Deliberately anchored on the keyword: the audit's first pass matched bare words and reported
/// `parameter`, `returns` and `here` as duplicate operation names, which are words inside
/// descriptions rather than definitions. A ratchet with false positives gets switched off.
fn operation_names(document: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in document.lines() {
        let line = line.trim_start();
        for keyword in ["query ", "mutation "] {
            if let Some(rest) = line.strip_prefix(keyword) {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() && !name.starts_with("__") {
                    names.push(name);
                }
            }
        }
    }
    names
}

fn rust_files(dir: &Path, into: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return, // a tree without tests/ or target/ is not an error here
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, into);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            into.push(path);
        }
    }
}

fn definitions() -> BTreeMap<String, Vec<String>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    // The workspace's own sources, not the build output or the fixtures: a name defined twice in a
    // test fixture is a different question from a name defined twice in the product.
    for sub in ["src", "crates/bridge/src"] {
        rust_files(&root.join(sub), &mut files);
    }

    let mut seen: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let shown = file
            .strip_prefix(&root)
            .unwrap_or(&file)
            .display()
            .to_string();
        for document in raw_strings(&text) {
            for name in operation_names(document) {
                let entry = seen.entry(name).or_default();
                if !entry.contains(&shown) {
                    entry.push(shown.clone());
                }
            }
        }
    }
    seen
}

#[test]
fn no_operation_name_is_defined_twice_except_the_ones_listed() {
    let duplicates: Vec<(String, Vec<String>)> = definitions()
        .into_iter()
        .filter(|(_, files)| files.len() > 1)
        .collect();

    let listed: Vec<&str> = KNOWN_DUPLICATES.iter().map(|(name, _)| *name).collect();
    let found: Vec<String> = duplicates.iter().map(|(name, _)| name.clone()).collect();

    let unexpected: Vec<&String> = found
        .iter()
        .filter(|name| !listed.contains(&name.as_str()))
        .collect();
    assert!(
        unexpected.is_empty(),
        "these operation names are defined in more than one file and are not in KNOWN_DUPLICATES:\n  {}\n\
         A name is a document's identity; two definitions of it can drift into two valid but \
         different queries. Either use the existing definition or give the new one its own name.",
        unexpected
            .iter()
            .map(|name| {
                let places = duplicates
                    .iter()
                    .find(|(candidate, _)| candidate == *name)
                    .map(|(_, files)| files.join(", "))
                    .unwrap_or_default();
                format!("{name}: {places}")
            })
            .collect::<Vec<_>>()
            .join("\n  ")
    );

    // The list may only shrink: an entry that no longer has a second definition is stale and must be
    // deleted, so the list cannot quietly become a record of things that are already fixed.
    let stale: Vec<&str> = listed
        .iter()
        .filter(|name| !found.iter().any(|found| found == *name))
        .copied()
        .collect();
    assert!(
        stale.is_empty(),
        "KNOWN_DUPLICATES lists names that are no longer defined twice, so the entry is stale and \
         should be removed: {stale:?}"
    );
}

#[test]
fn the_ratchet_detects_a_second_definition() {
    // The test's own subject, pinned: if `operation_names` stops finding names, or `raw_strings`
    // stops finding documents, the test above passes for the wrong reason - the empty-list failure
    // mode this file was written to avoid.
    let document = "query Alpha($x: String!) {\n  thing(id: $x) { id }\n}\n";
    assert_eq!(operation_names(document), vec!["Alpha".to_string()]);

    let source = "const A: &str = r#\"\nquery Alpha { id }\n\"#;\n";
    let found: Vec<&str> = raw_strings(source);
    assert_eq!(found.len(), 1);
    assert_eq!(operation_names(found[0]), vec!["Alpha".to_string()]);

    // And a word that merely looks like a definition is not one.
    let prose = "/// `query returns the parameter`\nquery Beta { id }\n";
    assert_eq!(operation_names(prose), vec!["Beta".to_string()]);
}
