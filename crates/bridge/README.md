# linear-bridge

The service half of `linear-cli-rs`: webhook intake, a durable queue, and the
platform-neutral vocabulary that a sync is written against.

It is a library, not a binary. The only binary in this workspace is `linear`, and
`linear webhook serve` is the entry point; everything here is also callable from a
test or from an agent.

## Layering, and the rule that keeps it

```text
http  ->  connector::Source  ->  domain::Event  ->  store  ->  queue  ->  Handler
                                                                             |
                                                            sink::Sink  <----+
```

| Module | Owns | Must not |
| --- | --- | --- |
| `domain` | the vocabulary: entities, events, capabilities, secrets | do I/O, name a platform |
| `connector` | the `Source` trait, signature schemes, `Reject` | know about HTTP, storage or a platform |
| `sources/*` | one platform each: parse + authenticate a delivery | write to a store, call another platform |
| `sink` | the `Sink` trait and the engine that executes a `SinkSpec` | decide *what* to write - that is the mapping |
| `http` | routing, body limits, status codes | contain reconciliation logic |
| `store` | durable state: deliveries, links, migrations | know what a delivery means |
| `queue` | claim / retry / park, and the `Handler` seam | know what a handler does |
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
secret_env = "FORGEJO_WEBHOOK_SECRET"   # what it signs its deliveries with
token_env = "FORGEJO_TOKEN"             # what this bridge reads and writes with
api_url = "http://127.0.0.1:3000/api/v1"   # optional; defaults to the preset's
closed_state = ["closed"]               # how this platform says "finished"
open_state = "open"

[platform.linear]
type = "linear"
secret_env = "LINEAR_WEBHOOK_SECRET"
token_env = "LINEAR_API_KEY"
closed_state = ["Done", "Canceled"]
open_state = "In Progress"
initial_state = "Todo"                  # where a newly mirrored issue lands

[[mapping]]
name = "linear-cli-rs"
source = "linear:VED"
sink = "forgejo:Vedaru/linear-cli-rs"
direction = "both"                      # or `oneway`
```

### Without a service

A deployment that only pushes changes holds no webhook secret at all - just the
credential it writes with, and a store to remember what it has already paired:

```toml
# cli-only.toml - no `bind`, because nothing is served
[bridge]
store = "linear-bridge.db"

[platform.linear]
type = "linear"
token_env = "LINEAR_API_KEY"
closed_state = ["Done", "Canceled"]
open_state = "In Progress"
initial_state = "Todo"

[platform.forgejo]
type = "forgejo"
token_env = "FORGEJO_TOKEN"
closed_state = ["closed"]
open_state = "open"

[[mapping]]
name = "linear-cli-rs"
source = "linear:VED"
sink = "forgejo:Vedaru/linear-cli-rs"
```

```sh
linear sync --config cli-only.toml            # dry run: print the plan, write nothing
linear sync --config cli-only.toml --apply    # bring the two ends into agreement
```

The same file drives both halves of the engine, so a sweep and a delivery can never
disagree about what to write. Hand *this* file to `webhook serve` and it refuses,
naming the platform and the variable it is missing: a secret is what an endpoint
*verifies* a delivery with, and a sweep verifies nothing. `secret_env` is required
exactly where a platform is asked to receive.

Two credentials per platform, and they are not interchangeable: `secret_env` is what
the platform signs *its* deliveries with (verified before anything is stored), and
`token_env` is what this bridge authenticates *to* the platform with (what it reads
and writes issues through). A platform with no token is still a valid connector - it
just cannot take part in a mapping, and the config says so at startup rather than on
the first webhook.

The state names are a fact about a workspace, not about Linear: which state means
"finished" differs per team, so it is configured per platform. A state list rather
than a single name, because a workflow usually has more than one way of being closed.

```sh
LINEAR_WEBHOOK_SECRET=... FORGEJO_WEBHOOK_SECRET=... \
  LINEAR_API_KEY=... FORGEJO_TOKEN=... \
  linear webhook serve
linear webhook serve --check          # resolve the config, print it, do not bind
linear webhook serve --bind 127.0.0.1:9000
```

`--check` is worth running before a deployment: it builds the sinks, resolves every
mapping with both platforms' vocabularies, and prints the result (secrets redacted) -
so a mapping that could not run fails there instead of on the first delivery.

Then point a webhook at `http://<host>:8787/webhooks/<platform>`: `linear` and
`forgejo` for the two connectors above. `GET /healthz` reports store liveness and
the delivery counts; `GET /version` reports the build.

Logging is `LINEAR_BRIDGE_LOG` (`error|warn|info|debug|trace|off`, default
`info`) to stderr, deliberately without timestamps: the supervisor adds those.

## Operational shape

- **Intake never does the work.** Verify -> persist -> `202`. Real work happens
  off the request path, so a slow sync cannot make a provider time out and retry.
- **A webhook is a notification, not the truth.** The reconciler reads both sides
  before deciding anything: the payload is a snapshot from whenever the provider
  queued it, and an API that was briefly unavailable has been sending stale
  snapshots ever since.
- **The link row is what makes an echo recognisable.** Every write records the
  content key of what was written, so the webhook that write provokes is a no-op
  rather than a loop.
- **Idempotent by key.** `(connector, delivery id)` is unique; a provider retry
  is a no-op insert answering `202 {"duplicate":true}`. A push delivery fans out
  into several events under one key, which is why the row stores the raw body.
- **The queue is the database.** A restart loses nothing: a delivery claimed by a
  crashed worker is reclaimed when its lease expires. Failures retry with
  exponential backoff and full jitter, then park as `dead` for an operator.
- **Handlers must be idempotent.** The queue retries; a handler that assumes
  "called once per change" duplicates work on the far side.
- **A misconfigured worker fails closed.** If a worker cannot be given a working
  reconciler it refuses deliveries (they retry, then park as dead) rather than
  acknowledging work it did not do - a silent "everything is fine" is the one
  failure mode a mirror must not have.
- **Bounded by construction.** Bodies are capped while reading (413 before the
  whole payload is in memory), a worker holds one delivery at a time, and both
  thread pools are fixed size.
- **No async runtime.** A request-per-thread server plus a small worker pool; the
  same blocking client the CLI already uses. One concurrency model, no runtime to
  size, and the shipped binary stays small.

## Adding a platform

A platform is a **preset file**, not a module. `presets/forgejo.toml` is the whole
forge connector - the webhook half *and* the write half - and adding Codeberg or
an internal service is copying it. The two halves of a preset:

- **the read half** (top level): `[signature]` (where the signature lives),
  `[delivery]`/`[event]` (where the ids and the event name live), `[[event.rule]]`
  (how a payload becomes an event, by JSON pointer), `[freshness]` (the replay
  bound, where the platform signs a timestamp), `[capabilities]`.
- **the API half** (`[sink]`, which holds reads as well as writes): `base_url`,
  `[sink.auth]`, the issue operations
  (`create`, `update`, `fetch`, `delete`, `transition`, `labels`, `attach`),
  `[sink.issue.read]`, `[sink.issue.lookup.*]` for turning a name into whatever id
  the platform wants, and `[sink.issue.comment]` - `create` plus, where the platform
  can, `update` and `delete`. Comment *edits* are addressed by the comment's own id,
  which is why a mirrored comment is paired in the link store as an entity of its own
  (kind `Comment`, deliberately with no content key: a comment is not part of the
  issue's revision, so recording it as one would make the next issue edit look like a
  change). A preset that declares no `comment.update` is not a bug - the mapping that
  needs one hears it by name rather than posting the edit as a second comment.
- **enumeration** (`[sink.issue.list]`): the request that lists a scope, the pointers
  *inside each item* (unlike `[sink.issue.read]`, which reads one issue from the root of
  a single-issue response), and how to walk to the next page - a page number, where a
  short page ends the walk, or a cursor with the platform's own `hasNextPage`. This is
  what a *sweep* starts from, as opposed to a delivery. Whether a platform can be swept
  is derived from this section rather than declared separately (`capabilities.list`), so
  the two cannot disagree, and a platform without it refuses by name - "there is nothing
  there" and "I cannot look" must not be the same answer to a caller about to create
  things.

Nothing else changes: intake, the queue, the store, the status codes and the
write path are already platform-agnostic. A new platform is validated at load
(`linear webhook serve --check`), so a typo is a startup error rather than a
field that mysteriously stops syncing.

What a preset cannot express, the engine refuses to guess:

- an operation the preset does not declare (`attach` on a forge) is
  `Error::Unsupported` naming the connector, not a silently skipped step;
- a request body may only use the neutral directives (`$title`, `$label_ids`,
  `$state_id`, ...); a typo is a validation error;
- a `$field` with no value **drops its key**, so a mirror never clears a field
  it merely has nothing to say about - `$field!` is how a preset asks for an
  explicit null;
- a failure inside a successful response (GraphQL's `errors` under `200 OK`) is a
  failure: `error_pointer` says where to look.
