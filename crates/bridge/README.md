# linear-bridge

The service half of `linear-cli-rs`: webhook intake, a durable queue, and the
platform-neutral vocabulary that a sync is written against.

It is a library, not a binary. The only binary in this workspace is `linear`, and
`linear webhook serve` is the entry point; everything here is also callable from a
test or from an agent.

## Layering, and the rule that keeps it

```text
http  ->  connector::Source  ->  domain::Event  ->  store  ->  queue  ->  Handler
```

| Module | Owns | Must not |
| --- | --- | --- |
| `domain` | the vocabulary: entities, events, capabilities, secrets | do I/O, name a platform |
| `connector` | the `Source` trait, signature schemes, `Reject` | know about HTTP, storage or a platform |
| `sources/*` | one platform each: parse + authenticate a delivery | write to a store, call another platform |
| `store` | durable state: deliveries, links, migrations | know what a delivery means |
| `queue` | claim / retry / park, and the `Handler` seam | know what a handler does |
| `http` | routing, body limits, status codes | contain reconciliation logic |
| `config` | the `[bridge]`/`[platform.*]`/`[[mapping]]` sections | hold a secret in a file it writes |

The rule: **the dependency arrows only point right**. `domain` depends on nothing
in this crate; `http` may depend on all of it. A change that makes `store` import
`http` is a change that has broken the design, not one that needs a new module.

Platform-agnostic is not a slogan here - it is testable. `rg -i 'linear|forgejo' crates/bridge/src/{domain,connector,store,queue,http}` must return nothing
(the only mentions allowed are in `sources/`, `config.rs` and doc comments).

## Running it

```toml
# linear.toml, next to your api_key - one file for CLI and service
[bridge]
bind = "127.0.0.1:8787"
store = "linear-bridge.db"

[platform.forgejo]
type = "forgejo"
secret_env = "FORGEJO_WEBHOOK_SECRET"

[platform.linear]
type = "linear"
secret_env = "LINEAR_WEBHOOK_SECRET"

[[mapping]]
name = "linear-cli-rs"
source = "linear:VED"
sink = "forgejo:Vedaru/linear-cli-rs"
```

```sh
LINEAR_WEBHOOK_SECRET=... FORGEJO_WEBHOOK_SECRET=... linear webhook serve
linear webhook serve --check          # resolve the config, print it, do not bind
linear webhook serve --bind 127.0.0.1:9000
```

Then point a webhook at `http://<host>:8787/webhooks/<platform>`: `linear` and
`forgejo` for the two connectors above. `GET /healthz` reports store liveness and
the delivery counts; `GET /version` reports the build.

Logging is `LINEAR_BRIDGE_LOG` (`error|warn|info|debug|trace|off`, default
`info`) to stderr, deliberately without timestamps: the supervisor adds those.

## Operational shape

- **Intake never does the work.** Verify -> persist -> `202`. Real work happens
  off the request path, so a slow sync cannot make a provider time out and retry.
- **Idempotent by key.** `(connector, delivery id)` is unique; a provider retry
  is a no-op insert answering `202 {"duplicate":true}`. A push delivery fans out
  into several events under one key, which is why the row stores the raw body.
- **The queue is the database.** A restart loses nothing: a delivery claimed by a
  crashed worker is reclaimed when its lease expires. Failures retry with
  exponential backoff and full jitter, then park as `dead` for an operator.
- **Handlers must be idempotent.** The queue retries; a handler that assumes
  "called once per change" duplicates work on the far side.
- **Bounded by construction.** Bodies are capped while reading (413 before the
  whole payload is in memory), a worker holds one delivery at a time, and both
  thread pools are fixed size.
- **No async runtime.** A request-per-thread server plus a small worker pool; the
  same blocking client the CLI already uses. One concurrency model, no runtime to
  size, and the shipped binary stays small.

## Adding a platform

1. Add a module under `src/sources/` implementing `Source`: `id`, `signature`
   (the header names, so the one verification path knows where to look),
   `parse` (native payload -> `Vec<Event>`, empty for "nothing to do"), and
   `capabilities` (what the platform can actually carry).
2. Wire `type = "<name>"` into `PlatformKind::parse`.
3. If it can be written to as well, add a `Sink` implementation beside it.

Nothing else changes: intake, the queue, the store and the status codes are
already platform-agnostic. Until `Sink` has its first implementation (M3) the
trait is deliberately absent rather than declared empty.
