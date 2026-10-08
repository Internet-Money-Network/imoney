#!/bin/bash
# Startup script for a Google Cloud seed node. It runs as root on every boot and only does
# what is still missing: swap, Docker, the node image built from the public repository, and
# the node container. A seed node verifies and relays; it never mines.
#
# The peers to connect to come from the instance's "imn-peers" metadata value
# (comma-separated host:port).
set -euo pipefail

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
fi

if ! docker ps -a --format '{{.Names}}' | grep -q '^imoney-node$'; then
  PEERS=$(curl -s -H 'Metadata-Flavor: Google' \
    'http://metadata.google.internal/computeMetadata/v1/instance/attributes/imn-peers' || true)
  # The peer-to-peer port is public; the API listens on this machine only
  docker run -d --name imoney-node --restart unless-stopped --memory 1500m \
    -v imoney-data:/data -p 18555:18555 -p 127.0.0.1:18556:18556 \
    imoney:latest ${PEERS:+--peers "$PEERS"}
fi
echo "imn-seed: ready"
