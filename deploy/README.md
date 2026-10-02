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

## What the unit already handles

- **Restarts.** `Restart=always`: the process is stateless between deliveries, and the queue is in
  the store, so a restart resumes rather than repeats.
- **Log rotation.** Logs go to the journal, which rotates them; nothing in the service writes a
  file that grows.
- **A network that comes and goes.** Deliveries are stored before they are worked on and an intake
  is idempotent by delivery id, so a provider retry - Linear retries, and so does a forge - is a
  no-op insert rather than a duplicate mirror.
