# Deploying the service

One binary, one config file, one environment file. The binary is the release asset; the config is
what `linear config service` printed, with the placeholders filled in; the environment file holds
the secrets the config only names.

The binary is a release asset. Fetch it over **loopback**, not over the public hostname: the
hostname is behind the login page, so a `curl` there downloads an HTML login form.

```sh
cd /tmp
base=$(curl -s http://127.0.0.1:3000/api/v1/repos/Vedaru/linear-cli-rs/releases/tags/rolling \
  | python3 -c 'import json,sys; print([a["browser_download_url"] for a in json.load(sys.stdin)["assets"] if a["name"].endswith(".tar.gz")][0])' \
  | sed 's|https://git.vedaru.cn|http://127.0.0.1:3000|')
curl -sSLO "$base" && curl -sSLO "$base.sha256"
sha256sum -c "$(basename "$base").sha256"
tar xzf "$(basename "$base")"
install -Dm755 "$(find . -maxdepth 2 -name linear -type f | head -1)" ~/.local/bin/linear
linear webhook --help        # "unrecognized subcommand" means an older binary is first on PATH
```

```sh
install -Dm644 deploy/linear-bridge.service      ~/.config/systemd/user/linear-bridge.service
install -Dm644 deploy/linear-bridge-sync.service ~/.config/systemd/user/linear-bridge-sync.service
install -Dm644 deploy/linear-bridge-sync.timer   ~/.config/systemd/user/linear-bridge-sync.timer
linear config service --team <KEY> --repo <owner>/<name> >> ~/.config/linear/linear.toml
install -Dm600 /dev/null ~/.config/linear-bridge/secrets.env  # then fill it in
systemctl --user daemon-reload
systemctl --user enable --now linear-bridge                   # Forgejo's deliveries
systemctl --user enable --now linear-bridge-sync.timer        # Linear's changes, pulled
loginctl enable-linger "$USER"                                # survive a logout, start at boot
```

`secrets.env` holds `LINEAR_API_KEY`, `FORGEJO_TOKEN`, `FORGEJO_WEBHOOK_SECRET` (you choose it;
the same value goes in the repository's webhook settings) and `LINEAR_WEBHOOK_SECRET` (Linear
gives you one when you create the webhook). The service wants the Linear secret even before any
route exists, because Linear is a platform this deployment *accepts deliveries for* - a missing
secret is a startup error by design, not a surprise on the first webhook.

### Which halves need which

| direction | what it needs |
| -- | -- |
| Linear -> forge | **nothing public**: the timer pulls every five minutes, and `Persistent=true` catches up after an outage |
| forge -> Linear | a webhook from the forge to `http://127.0.0.1:8787/webhooks/forgejo` - no gateway in the way |
| Linear -> forge, *pushed* | a route that reaches the service from outside, exempted from the login page - see below |

The first two are between machines you already run, and they are the ones worth getting working
first. The third is Linear calling *you*, and it is optional: the timer covers the same direction
by asking instead of being told.

Then, in order, the three things that actually decide whether it works:

**1. The route must not be behind the login page.** A webhook arrives as an unauthenticated POST,
so any gateway in front of it has to pass that path through. Measured on this deployment's hosts:
a webhook-shaped path on `git.vedaru.cn` and `monitor.vedaru.cn` answers **302 to an SSO login
page**, while `git.vedaru.cn/api/...` reaches Forgejo itself (a real `404` from the forge). A
platform posting to a 302'd route can record a *successful* delivery while nothing happened - it
followed the redirect, got a `200` and an HTML login page - so this must be checked **from
outside**, with `curl -i`, on the exact path Linear will use, and it must answer from the bridge
(`/healthz` returns the service's own body, not an HTML redirect).

```sh
curl -i https://<the bridge's host>/healthz          # must NOT be 302 to a login page
```

**2. Forgejo first, Linear second.** Local Forgejo can post straight to
`http://127.0.0.1:8787/webhooks/forgejo` with no gateway in the way, which makes it the end-to-end
test worth doing first: add the webhook in the repository's settings, watch the unit's journal,
and check the mirror. Linear's delivery then goes over the public route, which is the half that
depends on step 1.

**3. Watch it with the commands that exist for it.** `linear sync status` says whether the queue
drained or filled, and lists what it gave up on with the reason; `linear webhook replay <id>`
re-runs one against the body the provider sent, once the cause is fixed.

```sh
systemctl --user status linear-bridge        # the process
journalctl --user -u linear-bridge -f        # the log (the journal rotates it)
linear sync status --config ~/.config/linear/linear.toml
curl -s localhost:8787/healthz               # store liveness and delivery counts
```

## Running it as a container

`deploy/Dockerfile` and `deploy/compose.yaml`. The image is built from the **release asset**, not
from source: nothing is compiled in it, and the binary it carries is the one the pipeline tested.

```sh
cd ~/Projects/linear-cli-rs                    # or wherever the repo is
base=$(curl -s http://127.0.0.1:3000/api/v1/repos/Vedaru/linear-cli-rs/releases/tags/rolling \
  | python3 -c 'import json,sys; print([a["browser_download_url"] for a in json.load(sys.stdin)["assets"] if a["name"].endswith(".tar.gz")][0])' \
  | sed 's|https://git.vedaru.cn|http://127.0.0.1:3000|')
export RELEASE_URL="$base"
export RELEASE_SHA256="$(curl -s "$base.sha256" | cut -d' ' -f1)"
export CONFIG_FILE=~/.config/linear/linear.toml
export SECRETS_FILE=~/.config/linear-bridge/secrets.env
docker compose -f deploy/compose.yaml up -d
docker compose -f deploy/compose.yaml logs -f linear-bridge
```

Two things about this file are not style choices:

- **`network_mode: host`.** Forgejo posts to `http://127.0.0.1:8787` and the bridge calls Forgejo
  at `http://127.0.0.1:3000`, and both of those are only loopback *because* the container shares
  the host's network. Published ports (`-p 8787:8787`) put a NAT hop in the way, which is fine for
  a forge on the same host but is a second thing to explain when something does not arrive.
- **`network: host` for the build.** The release lives on the server's own Forgejo at
  `127.0.0.1:3000`, and inside a build container `127.0.0.1` is the *build's* loopback. The public
  hostname is not a workaround: it is behind the login page, so the download would fetch an HTML
  form and fail at the checksum - which is itself worth knowing, because that failure is loud only
  because the digest is verified.

The image is `debian:bookworm-slim` with the binary and `ca-certificates` in it, running as a
non-root user, with the store on a named volume (`linear-bridge-store`). Its `HEALTHCHECK` is
`linear sync status`: a container that cannot read its own queue is not healthy, however well the
process answers.

## Setting up the webhooks

Two webhooks, two directions, and only one of them needs anything public.

### Forgejo -> Linear (no gateway involved)

Repository → **Settings → Webhooks → Add webhook → Forgejo**:

| field | value |
| -- | -- |
| Target URL | `http://127.0.0.1:8787/webhooks/forgejo` |
| HTTP method | `POST` |
| Secret | the same value you put in `FORGEJO_WEBHOOK_SECRET` |
| Trigger on | Repository (Push), Issues, Issue comments, Pull requests |
| Active | yes |

Those four are exactly the events the preset models - `push`, `issues`, `issue_comment`,
`pull_request` arrive as the `X-Forgejo-Event` header, and nothing else is read.

Then press **Test delivery**. It sends a `ping`, which the bridge accepts (`202`) and correctly
mirrors as *nothing* - the journal says `delivery N carried no events`, and that is the success
case: if it says `bad signature` instead, the two secrets differ, and if nothing is logged at all,
the URL is not reaching the process.

After that, do something real: open an issue in the repository, then

```sh
linear sync status        # 1 done, 0 dead - and the issue is in your Linear team
```

### Linear -> Forgejo

Two ways, and the second is why nothing has to be public:

**Pulled (recommended first).** Nothing to configure on Linear's side: the `linear-bridge-sync`
timer (or the compose `linear-bridge-sync` service) runs `linear sync --apply` every five minutes
and brings Linear's changes across. Missing a delivery is not possible, because nothing is being
delivered.

**Pushed.** In Linear: **Settings → API → Webhooks → New webhook**. Set the URL to the route you
have exposed, copy the **signing secret** it shows into `LINEAR_WEBHOOK_SECRET`, and subscribe to
Issues (and Comments). Then, before trusting it:

```sh
curl -i https://<your route>/healthz     # the service's own body, NOT a 302 to a login page
```

Only after that does Linear's delivery mean anything - a `302` is followed by the platform, read
as a `200`, and filed as a successful delivery that never happened. Note also that Linear creates
the webhook before your route exists happily; it will simply report a failing webhook until the
route answers, so the order above is the one to use.

### When a delivery does not do what you expected

```sh
docker compose -f deploy/compose.yaml logs linear-bridge     # or: journalctl --user -u linear-bridge -f
linear sync status                                           # the queue, and what it gave up on
linear webhook replay <id>                                   # re-run one, on the stored body
```

## What the unit already handles

- **Restarts.** `Restart=always`: the process is stateless between deliveries, and the queue is in
  the store, so a restart resumes rather than repeats.
- **Log rotation.** Logs go to the journal, which rotates them; nothing in the service writes a
  file that grows.
- **A network that comes and goes.** Deliveries are stored before they are worked on and an intake
  is idempotent by delivery id, so a provider retry - Linear retries, and so does a forge - is a
  no-op insert rather than a duplicate mirror.
