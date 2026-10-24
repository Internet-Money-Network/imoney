# Seed nodes

The public test network (testnet-6) has four seed hostnames, all on P2P port 18555.

| Hostname | Address | Machine |
|---|---|---|
| `seed.internetmoneynetwork.org` | 103.91.16.143 | Not in Google Cloud: one of the owner's own machines on the home network (behind that public address). Managed by hand. |
| `seed1.internetmoneynetwork.org` | 35.232.251.39 | Google Cloud VM `imn-seed-us`, zone `us-central1-a`, `e2-small`. Serves the API publicly (`imn-api-public=1`, `imn-rpc-token` set). |
| `seed2.internetmoneynetwork.org` | 34.77.199.253 | Google Cloud VM `imn-seed-eu`, zone `europe-west1-b`, `e2-small` |
| `seed3.internetmoneynetwork.org` | 34.158.62.85 | Google Cloud VM `imn-seed-asia`, zone `asia-southeast1-b`, `e2-small` |

## Google Cloud

- Project ID `project-a70410ca-c896-45c7-90b` (project number 407162125572).
- Every VM boots with [`gcloud-seed-startup.sh`](gcloud-seed-startup.sh), stored as the instance's
  `startup-script` metadata. Keep that metadata in sync with the file in this repository whenever the
  script changes; a VM runs the copy in its metadata, not the one in git.
- Instance metadata: `imn-ref` (the release tag or commit to build; required, so a seed never
  builds whatever the default branch happens to hold), `imn-peers` (all three VMs list the four
  seeds), `imn-api-public`, `imn-rpc-token` and optionally `imn-origins` (only `imn-seed-us`). The
  token is a secret and is not recorded here. To upgrade, set `imn-ref` to the new tag, then
  rebuild as below.
- The Cloud Resource Manager API is disabled in the project, so `gcloud projects list` fails; address
  the project by its ID.

### Access from a Claude Code cloud session

The environment holds a network secret of type "GCP access token with service account key", scoped
to the Google API hosts (`*.googleapis.com`). The agent proxy adds the credentials to requests; the
key is never in the container. `gcloud` and plain `curl` against `compute.googleapis.com` both work.
The service account needs Compute Instance Admin (v1) and Service Account User.

SSH from a session is not used (it would add keys to project metadata). Resets go through the
startup script instead.

## The node image

From v0.1.10, CI builds the node's Docker image once for each release and publishes it as
`ghcr.io/internet-money-network/imoney:<tag>`; the release's `IMAGE-DIGEST.txt` names the exact
image by its content hash. A seed pulls it in about a minute instead of building for 20, and only
builds from source if no image was published for its `imn-ref`. On the main network, set
`imn-image` to the line in `IMAGE-DIGEST.txt`: the seed then runs that exact image or nothing.

The package must be public for seeds to pull without logging in: after the first release that
publishes it, set it to public once under the organisation's Packages, `imoney`, Package settings.

Resets that wipe every chain can do all three seeds at once: the old network is being retired
anyway, so there is nothing to keep running. Upgrades that keep the chain go one seed at a time.

## Resetting a Google Cloud seed (new network or wiped chain)

The startup script only builds the image and creates the container when they are missing, so a reset
means removing them and rebooting:

1. Set the instance's `startup-script` metadata to `gcloud-seed-startup.sh` with these lines added
   right after `set -euo pipefail`:

   ```bash
   if command -v docker >/dev/null; then
     docker rm -f imoney-node || true
     docker rmi -f imoney:latest || true
     docker volume rm imoney-data || true
   fi
   ```

2. Reset the VM (`gcloud compute instances reset NAME --zone ZONE --project project-a70410ca-c896-45c7-90b`).
   It clones `main`, builds the image (10 to 15 minutes on an `e2-small`, which has 3 GB of swap for
   this) and starts the node with an empty chain. The serial port output shows progress and ends with
   `imn-seed: ready`.
3. Once the node runs, set `startup-script` back to the plain `gcloud-seed-startup.sh`, so a later
   reboot does not wipe the chain again.

With SSH access to a VM, the same is: `sudo docker rm -f imoney-node; sudo docker rmi imoney:latest;
sudo docker volume rm imoney-data; sudo google_metadata_script_runner startup`.

Updating to new code without wiping the chain: remove only the container and the image, not the
volume.

## Resetting the home seed

On the machine running `seed.internetmoneynetwork.org`, stop the node, delete its data directory or
Docker volume, update the code to the current `main`, rebuild and start it again with the same
`--peers`. Port 18555 must stay forwarded to it on the router.
