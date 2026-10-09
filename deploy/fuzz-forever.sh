#!/bin/bash
# Runs every fuzz target in turn, an hour each, for as long as the container lives.
# The corpus and any crashing inputs are kept in /work, which should be a volume.
#
#   docker run -d --name imoney-fuzz --cpus 2 --memory 3g \
#     -v ~/imoney:/src:ro -v imoney-fuzz:/work rustlang/rust:nightly bash /src/deploy/fuzz-forever.sh
#
# A finding shows up as a file under /work/artifacts and a line in /work/findings.log.
set -u

command -v cargo-fuzz >/dev/null || cargo install cargo-fuzz --locked

# Work on a copy, so the build output does not land in the checkout
rm -rf /work/src
cp -r /src /work/src
rm -rf /work/src/target /work/src/fuzz/target
cd /work/src/fuzz
mkdir -p /work/corpus /work/artifacts

round=0
while true; do
  round=$((round + 1))
  for target in $(cargo fuzz list); do
    mkdir -p "/work/corpus/$target" "/work/artifacts/$target"
    echo "$(date -u +%FT%TZ) round $round: $target" >> /work/progress.log
    if ! CARGO_TARGET_DIR=/work/target cargo fuzz run "$target" "/work/corpus/$target" -- \
        -artifact_prefix="/work/artifacts/$target/" -max_total_time=3600 -rss_limit_mb=2048; then
      echo "$(date -u +%FT%TZ) FINDING in $target (round $round)" >> /work/findings.log
    fi
  done
done
