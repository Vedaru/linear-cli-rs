#!/bin/sh
# Does a public route actually reach the bridge?
#
#   deploy/check-route.sh https://<the bridge's host>
#
# The failure this exists for is silent. A gateway in front of the service can answer `302` to a
# login page; a platform that POSTs there *follows the redirect*, receives `200` and an HTML page,
# and records a **successful delivery while nothing happened**. So a `200` is not the signal - the
# service's own body is. Run this before creating a webhook, and again after any gateway change.
set -eu

base=${1:-}
[ -n "$base" ] || { echo "usage: $0 https://<host>" >&2; exit 2; }
base=${base%/}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail=0

probe() { # method path outfile -> status code
    method=$1
    path=$2
    out=$3
    curl -sS -m 20 -o "$out" -D "$out.headers" -w '%{http_code}' \
        -X "$method" "${base}${path}" 2>"$out.err" || echo 000
}

say() { printf '%s\n' "$*"; }

# 1. /healthz: the bridge's own JSON, or something in front of it.
code=$(probe GET /healthz "$tmp/health")
case "$code" in
301 | 302 | 307 | 308)
    say "FAIL  GET /healthz -> $code"
    say "      $(tr -d '\r' <"$tmp/health.headers" | grep -i '^location:' || true)"
    followed=$(curl -sS -m 20 -o "$tmp/followed" -L -w '%{http_code}' "${base}/healthz" 2>/dev/null || echo 000)
    say "      followed: $followed, $(wc -c <"$tmp/followed") bytes - exactly what a platform would"
    say "      record as a delivered webhook. The path (or host) needs exempting from the gateway."
    fail=1
    ;;
200)
    if grep -q '"status"' "$tmp/health" && grep -q '"deliveries"' "$tmp/health"; then
        say "ok    GET /healthz -> 200 $(cat "$tmp/health")"
    else
        say "FAIL  GET /healthz -> 200, but not the service's body: $(head -c 200 "$tmp/health")"
        fail=1
    fi
    ;;
*)
    say "FAIL  GET /healthz -> $code $(head -c 200 "$tmp/health" 2>/dev/null || true)"
    fail=1
    ;;
esac

# 2. An unsigned webhook: proves the *process* is the thing answering, and that it rejects what it
#    should. A signature failure is the success case here.
code=$(probe POST /webhooks/linear "$tmp/hook")
body=$(head -c 200 "$tmp/hook" 2>/dev/null || true)
case "$code" in
000)
    say "FAIL  POST /webhooks/linear -> no answer"
    fail=1
    ;;
3*)
    say "FAIL  POST /webhooks/linear -> $code (a redirect: the bridge never saw the request)"
    fail=1
    ;;
401 | 403)
    say "ok    POST /webhooks/linear -> $code $body"
    say "      (the bridge answered, and refused the missing signature)"
    ;;
4*)
    say "ok    POST /webhooks/linear -> $code $body"
    ;;
*)
    say "warn  POST /webhooks/linear -> $code $body"
    ;;
esac

if [ "$fail" = 0 ]; then
    say ""
    say "route reaches the bridge"
else
    say ""
    say "route does NOT reach the bridge - do not create a webhook against it yet"
fi
exit "$fail"
