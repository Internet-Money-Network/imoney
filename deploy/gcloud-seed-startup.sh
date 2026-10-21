#!/bin/bash
# Startup script for a Google Cloud seed node. It runs as root on every boot and only does
# what is still missing: swap, Docker, the node image built from the public repository, and
# the node container. A seed node verifies and relays; it never mines.
#
# Instance metadata it reads:
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
  rm -rf /opt/imoney
  git clone --depth 1 https://github.com/Internet-Money-Network/imoney.git /opt/imoney
  docker build -t imoney:latest /opt/imoney
  # Each build leaves its 2.7 GB build stage behind; a small disk fills after a few updates
  docker image prune -f
fi

if ! docker ps -a --format '{{.Names}}' | grep -q '^imoney-node$'; then
  PEERS=$(metadata imn-peers)
  TOKEN=$(metadata imn-rpc-token)
  ORIGINS=$(metadata imn-origins)
  ORIGINS=${ORIGINS:-https://internetmoneynetwork.org}
  API_BIND=127.0.0.1
  if [ "$(metadata imn-api-public)" = "1" ]; then API_BIND=0.0.0.0; fi
  docker run -d --name imoney-node --restart unless-stopped --memory 1500m \
    -v imoney-data:/data -p 18555:18555 -p "$API_BIND:18556:18556" \
    imoney:latest ${PEERS:+--peers "$PEERS"} ${TOKEN:+--rpc-bind 0.0.0.0:18556 --rpc-token "$TOKEN" --allowed-origins "$ORIGINS"}
fi
echo "imn-seed: ready"
