//! `linear issue start` - the branch flags, and what they promise.
//!
//! `--checkout` exists for compatibility with the other Rust CLI, where checking out a branch is
//! opt-in; here it is what `start` already does, so the flag changes nothing and says so in its
//! help. `--no-checkout` is its real counterpart: the same state change with no git at all.
//!
//! These tests pin the CLI contract rather than the request bodies. The half worth pinning is the
//! pair of flags and their conflict, because "accepts the other CLI's flag" and "can be told not to
//! touch the repository" are the two promises; the request the state change makes is upstream's and
//! already covered by the update tests.

mod common;

use common::{mock_env, run_cli, MockLinearServer};

#[test]
fn the_two_branch_flags_are_mutually_exclusive() {
    // No mocks: clap must refuse this before any request, so an empty server proves nothing was
    // sent rather than merely that nothing was answered.
    let server = MockLinearServer::start(vec![]);
    let output = run_cli(
        &["issue", "start", "VED-1", "--checkout", "--no-checkout"],
        &mock_env(&server),
    );

    assert!(!output.success());
    assert!(
        output.stderr.contains("--checkout") && output.stderr.contains("--no-checkout"),
        "the refusal names both: {}",
        output.stderr
    );
}

#[test]
fn the_help_says_checkout_is_already_the_default() {
    // The flag is accepted and inert, so the help has to be the place that says so - a flag which
    // looks like it turns something on, and does not, is worse than no flag.
    let server = MockLinearServer::start(vec![]);
    let output = run_cli(&["issue", "start", "--help"], &mock_env(&server));

    assert!(output.success(), "{}", output.stderr);
    assert!(output.stdout.contains("--checkout"), "{}", output.stdout);
    assert!(output.stdout.contains("--no-checkout"), "{}", output.stdout);
    assert!(
        output.stdout.contains("this IS the default") || output.stdout.contains("always creates"),
        "the help says the flag is already the behaviour: {}",
        output.stdout
    );
}
