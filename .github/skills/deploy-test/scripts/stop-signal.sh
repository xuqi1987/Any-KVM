#!/usr/bin/env bash
# stop-signal.sh — 停止远端信令服务器和 coturn
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/env.sh"

echo "==> 停止远端容器"
ssh_remote "
  cd /root/Any-KVM/deploy 2>/dev/null && docker-compose down 2>/dev/null \
  || docker rm -f any-kvm-signal any-kvm-coturn 2>/dev/null \
  || echo '容器已停止或不存在'
"
echo "✅ 完成"
