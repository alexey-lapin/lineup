#!/usr/bin/env bash
# Package a lineup-gateway binary with its example config, the pinned extractor requirements and
# a README, all at the archive's root.
#
#   package-gateway.sh BINARY ARCHIVE
set -euo pipefail

bin=$1 archive=$2
dir=$(mktemp -d)
cp "$bin" "$dir/lineup-gateway"
cp deploy/gateway.example.toml "$dir/gateway.example.toml"
cp deploy/gateway-tools-requirements.txt "$dir/gateway-tools-requirements.txt"
cp deploy/gateway-archive-README.md "$dir/README.md"
tar -C "$dir" -czf "$archive" lineup-gateway gateway.example.toml gateway-tools-requirements.txt README.md
