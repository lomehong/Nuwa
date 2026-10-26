#!/usr/bin/env bash
# Build the runner image matrix from a static (musl) nuwa binary.
#
#   bash docker/build.sh path/to/nuwa
#
# Defaults to the target-dir path produced by:
#   cargo build --release -p nuwa --target x86_64-unknown-linux-musl
set -euo pipefail
cd "$(dirname "$0")/.."

binary="${1:-target/x86_64-unknown-linux-musl/release/nuwa}"
if [ ! -f "$binary" ]; then
  echo "usage: $0 [path-to-static-nuwa]" >&2
  echo "not found: $binary" >&2
  exit 1
fi

cp "$binary" docker/nuwa
trap 'rm -f docker/nuwa' EXIT

docker build -f docker/runner-base.Dockerfile -t nuwa-runner-base:local .
for stack in go rust node python; do
  docker build -f "docker/runner-$stack.Dockerfile" -t "nuwa-runner-$stack:local" .
done

echo "built: nuwa-runner-base, nuwa-runner-{go,rust,node,python}:local"
