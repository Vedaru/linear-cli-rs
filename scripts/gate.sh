#!/bin/bash
# The gate, in the form that cannot lie to you.
#
# Every verdict below comes from an exit code. This exists because a version of it did not:
# a command that ended with `echo "(clippy: clean if empty)"` printed that line whether or
# not clippy had failed, and reading the label instead of the verdict is how a broken commit
# got pushed.
#
# Usage: scripts/gate.sh        (from the repository root)
set -u
cd "$(dirname "$0")/.."

# Wherever your toolchain lives, this finds it the way you would; a machine-specific path
# does not belong in a repository. Say so plainly rather than reporting three failures.
if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo is not on PATH: export PATH=<toolchain>/bin:\$PATH and run this again" >&2
  exit 2
fi

fail=0

cargo fmt --all --check >/tmp/gate-fmt.log 2>&1 \
  && echo "fmt: PASS" \
  || { echo "fmt: FAIL"; tail -20 /tmp/gate-fmt.log; fail=1; }

# `--test-threads=4`: an unbounded run oversubscribes the machines this is built on (four
# cores) and fails *wall-clock waits inside tests* rather than anything real.
cargo clippy --workspace --all-targets --locked -- -D warnings >/tmp/gate-clippy.log 2>&1 \
  && echo "clippy: PASS" \
  || { echo "clippy: FAIL"; grep -E "^(error|warning)" -A 8 /tmp/gate-clippy.log | head -40; fail=1; }

# The tests run with a throwaway HOME so a developer's own `~/.config` cannot decide the
# outcome - but the toolchain still has to be findable afterwards, or rustup reports "no
# default toolchain" and that reads exactly like a failing test. Hence the two explicit
# homes, captured before the override.
toolchain_home=${RUSTUP_HOME:-${HOME:-/root}/.rustup}
cargo_home=${CARGO_HOME:-${HOME:-/root}/.cargo}
env -u XDG_CONFIG_HOME HOME=$(mktemp -d) RUSTUP_HOME="$toolchain_home" CARGO_HOME="$cargo_home" \
  cargo test --workspace -- --test-threads=4 >/tmp/gate-test.log 2>&1 \
  && echo "tests: PASS" \
  || { echo "tests: FAIL"; grep -E "FAILED|panicked at|^error" -A 4 /tmp/gate-test.log | head -40; fail=1; }

grep -E "^test result" /tmp/gate-test.log | awk '{passed+=$4; failed+=$6} END {print "passed:", passed, " failed:", failed}'

if [ $fail -eq 0 ]; then echo "GATE: PASS"; else echo "GATE: FAIL"; fi
exit $fail
