# Deploying the bridge as a container

The service is one static binary, one config file and one environment file, so the container is
thin: `deploy/Dockerfile` compiles nothing and ships the **release asset** — the same artifact the
pipeline tested — on `debian:bookworm-slim`. This page is the whole runbook. `deploy/README.md`
stays the index for the two supported deployments (the user unit and this one).

| | systemd user unit | container |
| -- | -- | -- |
| picks up Linear's direction | `linear-bridge-sync.timer` on the host | `linear-bridge-sync` service in `deploy/compose.yaml`, or the host timer |
| picks up a Forgejo webhook | `127.0.0.1:8787` | `127.0.0.1:8787`, **host networking** |
| needs a route from outside | only for Linear's *pushed* direction | the same |
| needs a reachable base image | no | yes — see step 0 |
| log rotation | the journal | `max-size`/`max-file` in `deploy/compose.yaml` |
| store | `~/.local/share/linear-bridge/bridge.db` | the `linear-bridge-store` volume |

Run **one** of them, not both: with host networking they compete for the same `127.0.0.1:8787`, and
the loser exits with `address already in use`.

## 0. A base image you can actually pull

Measured from this network: `registry-1.docker.io`, `auth.docker.io` and `docker.io` all time out,
so `FROM debian:bookworm-slim` fails on a machine with no registry mirror. Configure one before
anything else, in `/etc/docker/daemon.json`:

```json
{
  "registry-mirrors": ["https://<your mirror>"]
}
```

```sh
sudo systemctl restart docker
docker pull debian:bookworm-slim        # the check; nothing below works until this does
```

The build stage additionally needs the release asset from the server's own Forgejo, which is
reachable at `127.0.0.1:3000` only *inside the host's network namespace* — that is why the build
declares `network: host`. A build container has its own loopback; without that declaration the
fetch reaches nothing, and the failure surfaces as a checksum error rather than a connection
error, because the digest is verified.

## 1. The asset and its digest, from the same release

`rolling` is republished on every push to `main`, so read the URL and the digest **together**, in
one go, from the release API — never a digest kept from an earlier build.

```sh
export RELEASE_URL="$(curl -s http://127.0.0.1:3000/api/v1/repos/Vedaru/linear-cli-rs/releases/tags/rolling \
  | python3 -c 'import json,sys; print([a["browser_download_url"] for a in json.load(sys.stdin)["assets"] if a["name"].endswith(".tar.gz")][0])' \
  | sed 's|https://git.vedaru.cn|http://127.0.0.1:3000|')"
export RELEASE_SHA256="$(curl -s "$RELEASE_URL.sha256" | cut -d' ' -f1)"

echo "$RELEASE_URL"       # .../linear-cli-rs-<sha>-x86_64-unknown-linux-gnu.tar.gz
echo "${RELEASE_SHA256:0:16}…"
```

Fetch over **loopback**, never the public hostname: the hostname is behind the login page, so a
`curl` there downloads an HTML login form. The payload is nested under a top-level directory named
after the artifact, which is why the Dockerfile `find`s the binary instead of assuming a flat
archive.

## 2. The config and the secrets

```sh
linear config service --team <KEY> --repo <owner>/<name> >> ~/.config/linear/linear.toml
install -Dm600 /dev/null ~/.config/linear-bridge/secrets.env    # then fill it in
```

`secrets.env` holds the four variables the config *names*: `LINEAR_API_KEY`, `FORGEJO_TOKEN`,
`LINEAR_WEBHOOK_SECRET` and `FORGEJO_WEBHOOK_SECRET`. Names must match — a config that says
`secret_env = "FORGEJO_WEBHOOK_SECRET"` and a file that defines `WEBHOOK_SECRET` fails at container
startup with the variable it could not find, which is the intended behaviour.

Two paths matter inside the container and both are absolute:

```toml
[bridge]
bind = "127.0.0.1:8787"                        # the host's loopback, because of `network_mode: host`
store = "/var/lib/linear-bridge/bridge.db"     # the named volume
```

`store` may also be left as the scaffold's `~/.local/share/linear-bridge/bridge.db`: `~` expands
against `$HOME`, and the image's user has `HOME=/var/lib/linear-bridge`, which *is* the volume. Say
which one you mean rather than relying on that.

## 3. Build and start

```sh
cd ~/Projects/linear-cli-rs
export CONFIG_FILE=~/.config/linear/linear.toml
export SECRETS_FILE=~/.config/linear-bridge/secrets.env

docker compose -f deploy/compose.yaml config    # parses and interpolates; the RELEASE_* vars must be set
docker compose -f deploy/compose.yaml build     # fetch, verify the digest, extract, install
docker compose -f deploy/compose.yaml up -d
```

`docker compose config` needs no daemon and is the first thing to run: it resolves the variables
the file requires (`${RELEASE_URL:?}` and friends) and prints the effective definition, so a typo in
the secrets path is caught before a build. Tag the image with the sha you built — the build args do
not pin it for you:

```sh
docker tag linear-bridge "linear-bridge:$(basename "$RELEASE_URL" | sed 's/.*linear-cli-rs-\([0-9a-f]*\)-.*/\1/')"
```

Two services come up: `linear-bridge` (the webhook receiver) and `linear-bridge-sync` (the same
five-minute sweep the systemd timer runs, with `--apply`). Keep both unless you have the host timer
running instead — in that case delete the `linear-bridge-sync` service, because two sweeps on one
store is two writers.

## 4. Verify it is up

```sh
docker compose -f deploy/compose.yaml ps
docker inspect --format '{{.State.Health.Status}}' linear-bridge      # starting -> healthy
docker compose -f deploy/compose.yaml logs --tail 20 linear-bridge
curl -s localhost:8787/healthz                                        # {"status":"ok","deliveries":{…}}
docker exec linear-bridge /usr/local/bin/linear sync status --config /etc/linear-bridge/linear.toml
```

The healthcheck is the binary's own (`linear sync status --config …`): it opens the store,
health-checks it and reads the counts, so a `healthy` container is one that can see its queue —
which is the property that matters after a restart. On first start the log prints the bind, the
endpoints per platform and the mapping (abridged; the paths are the ones in the config above):

```
Accepting webhooks on http://127.0.0.1:8787 (config /etc/linear-bridge/linear.toml, database /var/lib/linear-bridge/bridge.db, 0o600)
  POST http://127.0.0.1:8787/webhooks/forgejo   supports: title, body, assignees, list, labels, due_dates, references, states:open/closed
  POST http://127.0.0.1:8787/webhooks/linear   supports: title, body, assignees, list, labels, due_dates, priorities, deletion, states:named
  syncing linear:VED <-> forgejo:Vedaru/linear-cli-rs (both ways)
INFO  linear_bridge::http: bridge listening on 127.0.0.1:8787 (4 intake threads, 2 workers)
```

Then prove the intake rejects what it should, from the host (host networking means `localhost` is
the container's):

```sh
deploy/check-route.sh http://localhost:8787
#   ok    GET /healthz -> 200 {"status":"ok","deliveries":{"pending":0,"active":0,"done":0,"dead":0}}
#   ok    POST /webhooks/linear -> 400 {"error":"missing required header"}
#
#   route reaches the bridge
```

`400 missing required header` is the success case: the container answered and refused an unsigned
body. A `3xx`, a `200` carrying HTML, or `no answer` all mean the request never reached it.

## 5. Point the webhooks at it

**Forgejo first** — it is on this machine, so there is no gateway in the way. Repository →
**Settings → Webhooks → Add webhook → Forgejo**: target `http://127.0.0.1:8787/webhooks/forgejo`,
method `POST`, secret = `FORGEJO_WEBHOOK_SECRET`, triggers Issues / Issue comments / Pull requests
/ Push. **Test delivery** sends a ping: the container log says `delivery N carried no events`, which
is the success case — `bad signature` means the two secrets differ, silence means the URL is not
reaching the process.

**Linear** is the direction that needs something public, and it is optional: the sweep already
covers Linear → forge by asking instead of being told. If you do expose it, the route must not be
behind a login page, and the check is external:

```sh
deploy/check-route.sh https://<the route>     # exits non-zero unless the container answers
```

A `302` is followed by the platform, read as `200`, and filed as a delivery that never happened —
`deploy/README.md` carries the measured exemption boundary and the nginx location to add.

## 6. Operating it

```sh
docker compose -f deploy/compose.yaml logs -f linear-bridge          # json-file, capped, rotates
docker compose -f deploy/compose.yaml restart linear-bridge
docker inspect -f '{{.State.Health.Status}}' linear-bridge
```

- **Upgrades.** Re-read step 1 (new URL **and** new digest, together), then
  `docker compose -f deploy/compose.yaml build && … up -d`. The store is on a named volume, so the
  queue survives; a restart resumes rather than repeats, because an intake is idempotent by
  delivery id.
- **Rollback.** Build with the older release's URL and digest and `up -d` again. The store is not
  migrated between versions; if a schema change ever needs that, it is in the release notes, not
  here.
- **The store.** `docker volume inspect linear-cli-rs_linear-bridge-store` shows where it lives on
  the host. Back it up with the bridge image itself, which needs no extra pull:
  `docker run --rm --entrypoint tar -v linear-cli-rs_linear-bridge-store:/data -v "$PWD":/out linear-bridge czf /out/bridge-store.tgz -C /data .`
- **A network that comes and goes.** Deliveries are stored before they are worked on, and a provider
  retry is a no-op insert. The container does not need to notice the campus portal dropping.
- **Logs.** Both services cap `json-file` at 10 MB × 5. Without `logging:` a container's log grows
  until the disk does — the one thing the unit gets for free from the journal and this does not.

## The same thing without compose

```sh
docker build --network host -f deploy/Dockerfile \
  --build-arg RELEASE_URL="$RELEASE_URL" \
  --build-arg RELEASE_SHA256="$RELEASE_SHA256" \
  -t linear-bridge .

docker run -d --name linear-bridge \
  --network host --restart unless-stopped \
  --log-opt max-size=10m --log-opt max-file=5 \
  --env-file "$SECRETS_FILE" \
  -v "$CONFIG_FILE":/etc/linear-bridge/linear.toml:ro \
  -v linear-bridge-store:/var/lib/linear-bridge \
  linear-bridge

docker run -d --name linear-bridge-sync \
  --network host --restart unless-stopped \
  --log-opt max-size=10m --log-opt max-file=5 \
  --env-file "$SECRETS_FILE" \
  -v "$CONFIG_FILE":/etc/linear-bridge/linear.toml:ro \
  -v linear-bridge-store:/var/lib/linear-bridge \
  --entrypoint /bin/sh linear-bridge -c \
  'while true; do /usr/local/bin/linear sync --apply --config /etc/linear-bridge/linear.toml || true; sleep 300; done'
```

## When it does not work

| symptom | cause | what to do |
| -- | -- | -- |
| `Cannot connect to the Docker daemon` | you are not in the `docker` group, or the daemon is down | `sudo usermod -aG docker "$USER"` then re-login; `systemctl status docker` |
| build fails pulling `debian:bookworm-slim` | no reachable registry (Hub times out here) | step 0 — configure a mirror, then `docker pull` it by hand |
| build fails `curl: (7)` or `sha256sum: FAILED` | the asset was not reachable (the build needs `network: host`), or `rolling` moved between reading the URL and the digest | re-read **both** in one go (step 1); confirm the `.sha256` asset belongs to the tarball you downloaded |
| container exits at once, log says `needs the environment variable …` | the config names a secret that `secrets.env` does not define | make the `secret_env` names and the file's keys agree |
| container exits, `address already in use` on 8787 | the systemd unit (or another container) already binds it | run one deployment, or change `bind` in the config |
| `unhealthy` | `linear sync status` cannot open the store | check the volume is mounted at the store's *containing directory* and is writable by the image's `bridge` user |
| container fine, nothing mirrored | signature mismatch, a ping delivery, or a `302` route | container log for `bad signature`; `delivery N carried no events` for a ping; `deploy/check-route.sh` for the route |
| Linear says the delivery succeeded | the route answered a login page (`302` → `200` HTML) | `deploy/check-route.sh <url>` — fix the exemption before trusting any delivery |
| the sweep runs but writes nothing | the config is being read without `--apply` | the sync service and the timer both pass `--apply`; a bare `linear sync` is a dry run by design |

## Removing it

```sh
docker compose -f deploy/compose.yaml down          # add -v only if the queue is disposable
docker rmi linear-bridge
```

`down -v` deletes `linear-bridge-store`, which holds the delivery queue and every link between a
Linear issue and a Forgejo one. The webhooks keep pointing at a route that now answers nothing, so
delete them in Forgejo and Linear if this is not a reinstall.
