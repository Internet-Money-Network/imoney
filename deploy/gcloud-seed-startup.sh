#!/bin/bash
# Startup script for a Google Cloud seed node. It runs as root on every boot and only does
# what is still missing: swap, Docker, the node image, and the node container. The image is the
# one CI publishes for the release; building it here from source (about 20 minutes on an
# e2-small) is only the fallback. A seed node verifies and relays; it never mines.
#
# Instance metadata it reads:
#   imn-ref         the release tag to run (e.g. v0.1.9). Required: a seed never runs
#                   whatever the default branch holds at the moment it boots
#   imn-image       optional: the exact image to run, pinned by digest, as in the release's
#                   IMAGE-DIGEST.txt (ghcr.io/...@sha256:...). When set, nothing else is run,
#                   and nothing is built from source. Use it on the main network.
#   imn-peers       peers to connect to, comma-separated host:port
#   imn-api-public  "1" to serve the read-only API on port 18556 to the outside (the firewall
#                   decides who can reach it); otherwise the API listens on this machine only
#   imn-rpc-token   token required for submitting blocks, when the API is public
#   imn-origins     websites allowed to use the public API from a browser, comma-separated
#                   (default https://internetmoneynetwork.org, the site that proxies to it)
set -euo pipefail

metadata() {
  curl -sf -H 'Metadata-Flavor: Google' \
    "http://metadata.google.internal/computeMetadata/v1/instance/attributes/$1" || true
}

if [ ! -f /swapfile ]; then
  # Building the node needs more memory than a small instance has
  fallocate -l 3G /swapfile
  chmod 600 /swapfile
  mkswap /swapfile
  echo '/swapfile none swap sw 0 0' >> /etc/fstab
fi
swapon -a || true

if ! command -v docker >/dev/null; then
  apt-get update
  DEBIAN_FRONTEND=noninteractive apt-get install -y docker.io git
fi

if ! docker image inspect imoney:latest >/dev/null 2>&1; then
  REF=$(metadata imn-ref)
  if [ -z "$REF" ]; then
    echo "imn-seed: set instance metadata imn-ref to the release tag or commit to build" >&2
    exit 1
  fi
  # The image CI built for this release, or the one pinned by digest
  PINNED=$(metadata imn-image)
  IMAGE=${PINNED:-ghcr.io/internet-money-network/imoney:$REF}
  if docker pull -q "$IMAGE"; then
    docker tag "$IMAGE" imoney:latest
    echo "imn-seed: pulled $IMAGE ($(docker image inspect --format '{{index .RepoDigests 0}}' "$IMAGE"))"
  elif [ -n "$PINNED" ]; then
    echo "imn-seed: could not pull the pinned image $PINNED; not building anything else" >&2
    exit 1
  else
    echo "imn-seed: no published image for $REF; building it from source"
    rm -rf /opt/imoney
    git init -q /opt/imoney
    git -C /opt/imoney fetch -q --depth 1 https://github.com/Internet-Money-Network/imoney.git "$REF"
    git -C /opt/imoney checkout -q FETCH_HEAD
    echo "imn-seed: building $REF ($(git -C /opt/imoney rev-parse HEAD))"
    docker build -t imoney:latest /opt/imoney
    # Each build leaves its 2.7 GB build stage behind; a small disk fills after a few updates
    docker image prune -f
  fi
fi

if ! docker ps -a --format '{{.Names}}' | grep -q '^imoney-node$'; then
  PEERS=$(metadata imn-peers)
  TOKEN=$(metadata imn-rpc-token)
  ORIGINS=$(metadata imn-origins)
  ORIGINS=${ORIGINS:-https://internetmoneynetwork.org}
  API_BIND=127.0.0.1
  if [ "$(metadata imn-api-public)" = "1" ]; then API_BIND=0.0.0.0; fi
  # The token goes to the node as a file, not on its command line, where the process list
  # and the container's settings would show it. The directory is root's alone.
  TOKEN_ARGS=()
  if [ -n "$TOKEN" ]; then
    install -d -m 700 /opt/imn
    printf '%s' "$TOKEN" > /opt/imn/rpc-token
    chmod 444 /opt/imn/rpc-token
    TOKEN_ARGS=(-v /opt/imn/rpc-token:/run/imn/rpc-token:ro)
  fi
  docker run -d --name imoney-node --restart unless-stopped --memory 1500m \
    --label com.centurylinklabs.watchtower.enable=false \
    -v imoney-data:/data "${TOKEN_ARGS[@]}" -p 18555:18555 -p "$API_BIND:18556:18556" \
    imoney:latest ${PEERS:+--peers "$PEERS"} \
    ${TOKEN:+--rpc-bind 0.0.0.0:18556 --rpc-token-file /run/imn/rpc-token --allowed-origins "$ORIGINS"}
fi
echo "imn-seed: ready"
