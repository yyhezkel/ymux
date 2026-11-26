#!/usr/bin/env bash
# Rebake the embedded server blobs from a ci-windows run of HEAD.
#
#   scripts/rebake-server.sh [run-id]
#
# Without a run id it takes the newest ci-windows run of HEAD. It refuses a
# run built from another commit: the CI gate byte-compares the blobs with the
# build of the commit under test, so a blob from any other commit fails it.
# It commits only the two blobs and does not push (fetch first, Rule #15).
set -euo pipefail

die() { echo "rebake-server: $*" >&2; exit 1; }

root=$(git rev-parse --show-toplevel)
cd "$root"
res=app/src-tauri/resources
head=$(git rev-parse HEAD)
short=${head:0:8}

run=${1:-}
if [ -z "$run" ]; then
  run=$(gh run list --commit "$head" --workflow ci-windows.yml --limit 1 \
    --json databaseId --jq '.[0].databaseId // empty')
  [ -n "$run" ] || die "no ci-windows run for HEAD $short. Push, wait for the run, then try again."
fi

read -r sha status < <(gh run view "$run" --json headSha,status --jq '"\(.headSha) \(.status)"')
[ "$sha" = "$head" ] || die "run $run built ${sha:0:8}, HEAD is $short. Use a run of HEAD."
[ "$status" = "completed" ] || die "run $run is $status. Wait for it to finish."

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
gh run download "$run" -n ymux-server-linux -D "$tmp" \
  || die "run $run has no ymux-server-linux artifact (did go vet or go test fail?)"

same=1
for n in x64 arm64; do
  f="$tmp/ymux-server-linux-$n"
  [ -s "$f" ] || die "artifact is missing ymux-server-linux-$n"
  cmp -s "$f" "$res/ymux-server-linux-$n" || same=0
done
if [ "$same" = 1 ]; then
  echo "blobs already match run $run. Nothing to commit."
  exit 0
fi

# cp onto an existing file keeps its mode (100644 in git).
cp "$tmp/ymux-server-linux-x64" "$tmp/ymux-server-linux-arm64" "$res/"
git commit -q \
  -m "chore(server): rebake blobs from CI run $run" \
  -m "From the ymux-server-linux artifact of ci-windows run $run, built from $short." \
  -- "$res/ymux-server-linux-x64" "$res/ymux-server-linux-arm64"
echo "committed $(git rev-parse --short HEAD). Fetch, push, and the next run must pass the byte-compare."
