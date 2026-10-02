# The bridge as a container.
#
#   docker build --network host -f deploy/Dockerfile \
#     --build-arg RELEASE_URL="<asset url>" \
#     --build-arg RELEASE_SHA256="<the .sha256 file's digest>" \
#     -t linear-bridge .
#
# `--network host` is for the *build*: the release lives on the server's own Forgejo at
# `127.0.0.1:3000`, and inside a build container `127.0.0.1` is the build's own loopback, not the
# host's. The public hostname is not an alternative - it is behind a login page, so the download
# would succeed at fetching an HTML form and fail at untarring it.
#
# Nothing is compiled here. The image carries the release asset and nothing else: a compiler and
# a cargo registry have no business in a runtime image, and the release is the artifact that was
# tested.

FROM debian:bookworm-slim AS fetch
ARG RELEASE_URL
ARG RELEASE_SHA256
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /tmp
# The digest is required, not optional: a build that fetches an artifact it cannot verify is one
# that will happily ship the wrong binary.
RUN test -n "$RELEASE_URL" && test -n "$RELEASE_SHA256" \
 && curl -fsSL "$RELEASE_URL" -o linear.tgz \
 && echo "$RELEASE_SHA256  linear.tgz" | sha256sum -c - \
 && tar xzf linear.tgz \
 && install -Dm755 "$(find . -maxdepth 2 -name linear -type f | head -1)" /usr/local/bin/linear

FROM debian:bookworm-slim
# `ca-certificates` is the only runtime dependency: the platforms are HTTPS.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd -r bridge \
 && useradd -r -g bridge --home-dir /var/lib/linear-bridge --create-home bridge

COPY --from=fetch /usr/local/bin/linear /usr/local/bin/linear

USER bridge
WORKDIR /var/lib/linear-bridge

# Mount the config here, and the store wherever the config says (by default the working
# directory, which is why it is worth setting `store = "/var/lib/linear-bridge/bridge.db"`).
ENV CONFIG=/etc/linear-bridge/linear.toml

EXPOSE 8787
ENTRYPOINT ["/usr/local/bin/linear", "webhook", "serve"]
CMD ["--config", "/etc/linear-bridge/linear.toml"]

# The check is the binary's own: `sync status` opens the store, health-checks it and reads the
# counts, and fails if any of that cannot be done. A container that cannot see its queue is not
# healthy, however well the process is answering.
HEALTHCHECK --interval=60s --timeout=15s --start-period=10s \
  CMD ["/usr/local/bin/linear", "sync", "status", "--config", "/etc/linear-bridge/linear.toml"]
