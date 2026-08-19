#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

browser_host=crates/lspf/examples/worker_channel_hosts/browser
node_host=crates/lspf/examples/worker_channel_hosts/node

npm --prefix "$browser_host" run build
npm --prefix "$node_host" run build
npm --prefix "$node_host" run smoke
