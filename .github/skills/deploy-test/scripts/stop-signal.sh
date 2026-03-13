#!/usr/bin/env bash
# stop-signal.sh — 停止远端信令服务器和 coturn
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/env.sh"

REMOTE_REPO="/opt/any-kvm"

echo "==> 停止远端容器"
ssh_remote "
  docker compose -f ${REMOTE_REPO}/deploy/docker-compose.yml down 2>/dev/null \
  || docker stop any-kvm-signal any-kvm-coturn 2>/dev/null \
  || echo '容器已停止或不存在'
"
echo "✅ 完成"
