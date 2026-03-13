#!/usr/bin/env bash
# build-agent.sh — 在本机编译 any-kvm-agent（release）
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/env.sh"

echo "==> [1/1] 编译 any-kvm-agent (release, features: v4l2-capture + screen-capture)"
cd "${AGENT_DIR}"
cargo build --release
echo "✅ 编译完成: ${AGENT_BIN}"
ls -lh "${AGENT_BIN}"
