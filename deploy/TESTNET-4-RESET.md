# Testnet-4 reset runbook

Restarts the public test network as **testnet-4** from release **v0.1.5**. Needed because the
difficulty rule changed in `ac853f1` (effective-time window): a node syncing from scratch rejects
the testnet-3 chain at about block 11. Nothing here runs until the owner says so, step by step.

**Gates:** the reset must happen before *any* of these: a Google Cloud seed is rebuilt, a new
node joins, or anyone mines.

Testnet-4 has a new genesis block (2026-10-22 00:00 UTC), so old testnet-3 nodes are refused at
the handshake instead of offering stale blocks. Test coins are void; nothing of value is lost.

## What changes and what is kept

| Thing | Action |
|---|---|
| Chain data, all four nodes (`imoney-data` volumes) | **Wiped** |
| Pool balances (`pool-state.json` in `imoney-pool`) | **Wiped**; the pool recreates it empty |
| Pool key (`pool-key.hex`) | Kept |
| Node mining key (`miner-key.hex`, if present) | Kept |
| RPC token (home node and pool, `imn-seed-us`) | **Rotated**; the old one was exposed in `docker inspect` and in a session log |
| `imoney-fuzz` container | Untouched (chain-independent) |
| Other containers on the home server | Untouched |
| Container limits and Watchtower opt-out on the home server | **Restored** (lost in an earlier recreate) |

## 0. Release

From a machine with push access, once the runbook is approved:

```bash
git checkout l1-foundation && git pull
git tag -a v0.1.5 -m "testnet-4" <commit that adds this file>
git push origin v0.1.5
```

The tag must contain `ac853f1` (round-3 fixes), the testnet-4 genesis, `--rpc-token-file`, and the
updated `deploy/gcloud-seed-startup.sh` (pinned `imn-ref`, token file). CI publishes binaries for
the tag.

## 1. Home seed (`seed.internetmoneynetwork.org`, `ssh timpisynaptron@192.168.1.69`)

```bash
# Build the release, keeping the current image under another name in case it is needed
cd ~/imoney && git fetch --tags origin && git checkout v0.1.5
docker tag imoney:latest imoney:pre-testnet4
docker build -t imoney:v0.1.5 -t imoney:latest .

# Stop and remove the pool and the node (the pool first: it talks to the node)
docker stop imoney-pool imoney-node && docker rm imoney-pool imoney-node

# Keep the node's mining key, if it has one, then wipe the chain
mkdir -p ~/imn-backup
docker run --rm -v imoney-data:/d -v ~/imn-backup:/b alpine sh -c 'cp -p /d/miner-key.hex /b/ 2>/dev/null || true'
docker volume rm imoney-data && docker volume create imoney-data
docker run --rm -v imoney-data:/d -v ~/imn-backup:/b alpine sh -c 'if [ -f /b/miner-key.hex ]; then cp -p /b/miner-key.hex /d/ && chown 999:999 /d/miner-key.hex; fi'

# Void the pool's balances; keep its key
# (and the stale pre-testnet-3 backup, which holds void accounting)
docker run --rm -v imoney-pool:/d alpine sh -c 'rm -f /d/pool-state.json /d/pool-state.json.partial /d/pool-state.json.pre-testnet3.bak'

# A new RPC token, kept in a file only the owner can reach
install -d -m 700 ~/imn
openssl rand -hex 32 > ~/imn/rpc-token && chmod 444 ~/imn/rpc-token

# Node: 2 CPUs, 3 GB, no Watchtower, API on this machine only, token from the file
docker run -d --name imoney-node --restart unless-stopped --cpus 2 --memory 3g \
  --label com.centurylinklabs.watchtower.enable=false \
  -v imoney-data:/data -v ~/imn/rpc-token:/run/imn/rpc-token:ro \
  -p 18555:18555 -p 127.0.0.1:18556:18556 \
  imoney:v0.1.5 --rpc-bind 0.0.0.0:18556 --rpc-token-file /run/imn/rpc-token \
  --peers seed1.internetmoneynetwork.org:18555,seed2.internetmoneynetwork.org:18555,seed3.internetmoneynetwork.org:18555

# Pool: 1 CPU, 768 MB, host network, working directory /data (it reads pool-key.hex there)
docker run -d --name imoney-pool --restart unless-stopped --cpus 1 --memory 768m \
  --label com.centurylinklabs.watchtower.enable=false \
  --network host -w /data -v imoney-pool:/data -v ~/imn/rpc-token:/run/imn/rpc-token:ro \
  --entrypoint imoney-stratum imoney:v0.1.5 \
  --node http://127.0.0.1:18556 --rpc-token-file /run/imn/rpc-token \
  --pool --fee-percent 1.5 --min-payout 1 \
  --bind 0.0.0.0:18557 --status-bind 0.0.0.0:18558 --public-address pool.internetmoneynetwork.org:18557
```

The node's image entrypoint already adds `--data-dir /data --p2p-bind 0.0.0.0:18555`. The
FishHash dataset for the new genesis is rebuilt on first start (a few minutes).

**Check:** `curl -s localhost:18556/api/v1/info` shows `"network":"testnet-4"`, `total_blocks` 1,
and a genesis hash matching the other nodes. `docker inspect imoney-node --format '{{.Args}}'` shows no token.
`curl -s localhost:18558/api/stats` answers.

## 2. Google Cloud seeds

PowerShell on the owner's PC, with `gcloud` signed in to the seed account. Project
`project-a70410ca-c896-45c7-90b`. Instances: `imn-seed-us` (us-central1-a), `imn-seed-eu`
(europe-west1-b), `imn-seed-asia` (asia-southeast1-b). One at a time.

For each instance:

1. Pin the release, and on `imn-seed-us` only, a new token:

   ```powershell
   gcloud compute instances add-metadata imn-seed-us --zone us-central1-a --project project-a70410ca-c896-45c7-90b --metadata imn-ref=v0.1.5
   # imn-seed-us only: a fresh token, never written down anywhere else
   $t = -join ((1..32) | ForEach-Object { '{0:x2}' -f (Get-Random -Maximum 256) })
   gcloud compute instances add-metadata imn-seed-us --zone us-central1-a --project project-a70410ca-c896-45c7-90b --metadata imn-rpc-token=$t
   ```

2. Set `startup-script` to `deploy/gcloud-seed-startup.sh` **from the v0.1.5 tag** with the wipe
   lines from `deploy/SEEDS.md` ("Resetting a Google Cloud seed") inserted after
   `set -euo pipefail`, then reset the VM:

   ```powershell
   gcloud compute instances add-metadata imn-seed-us --zone us-central1-a --project project-a70410ca-c896-45c7-90b --metadata-from-file startup-script=seed-startup-wipe.sh
   gcloud compute instances reset imn-seed-us --zone us-central1-a --project project-a70410ca-c896-45c7-90b
   ```

3. Watch the serial output for `imn-seed: building v0.1.5 (<commit>)` and then `imn-seed: ready`
   (10 to 15 minutes):

   ```powershell
   gcloud compute instances get-serial-port-output imn-seed-us --zone us-central1-a --project project-a70410ca-c896-45c7-90b | Select-String "imn-seed"
   ```

4. Set `startup-script` back to the plain `gcloud-seed-startup.sh` from the tag, so a reboot does
   not wipe the chain again.

The new startup script refuses to build without `imn-ref` (N-24), passes the token as a file,
and labels the container out of Watchtower.

## 3. Website

The explorer and wallet must match the nodes:

```bash
python scripts/build-site.py --apps-from v0.1.5
cd dist-site && npx wrangler pages deploy public --project-name internetmoneynetwork --branch main --commit-dirty=true
```

Outward-facing: run only with the owner's go-ahead.

## 4. Verify the network

- All four nodes report `testnet-4` and the same `genesis_hash`.
- `curl -s localhost:18556/api/v1/peers` on the home node lists the seeds; at least one link is
  outbound.
- The website's explorer shows block 0 / 1, and its `/api` answers through Cloudflare.
- The pool status page loads.

## 5. First blocks

Nothing mines until someone chooses to. To start, mine from the GPU through
`START-HOME-NODE-MINER.bat` (gitignored). Block submission needs the **new** token: update it in
that file from `~/imn/rpc-token` on the home server. Never mine on the Google Cloud seeds.

## Afterwards

- Update the session memory and `deploy/SEEDS.md` with the new container flags.
- Ask the tester to confirm N-24 closed (seeds built from a pinned tag).
- Delete `imoney:pre-testnet4` once testnet-4 has run for a day.
- Optional tidy-up: `docker volume ls -qf dangling=true` on the home server lists anonymous volumes
  left by earlier container recreates. Check each with `docker volume inspect` before removing it;
  never remove `imoney-data`, `imoney-pool`, `imoney-fuzz` or another app's named volume.

## Rollback

The chain is a test chain, so there is nothing to roll back to. If v0.1.5 will not start, run
`imoney:pre-testnet4` with the same flags but `--rpc-token "$(cat ~/imn/rpc-token)"` in place of
`--rpc-token-file` (the old image does not know the file option; use the new token, the old one
is retired), and report the error. That image serves the testnet-3 genesis, so after the wipe it
is for diagnosing only: it cannot rejoin anything.
