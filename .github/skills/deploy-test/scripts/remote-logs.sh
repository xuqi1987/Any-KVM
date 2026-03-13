#!/usr/bin/env bash
# remote-logs.sh — 查看远端容器日志（signal + coturn）
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/env.sh"

LINES="${1:-50}"

echo "==> 信令服务器日志 (最后 ${LINES} 行)"
ssh_remote "cd /root/Any-KVM/deploy && docker-compose logs --tail=${LINES} signal 2>/dev/null || docker logs any-kvm-signal --tail=${LINES} 2>/dev/null || echo '容器不存在'"

echo ""
echo "==> coturn 日志 (最后 ${LINES} 行)"
ssh_remote "docker logs any-kvm-coturn --tail=${LINES} 2>/dev/null || echo '容器不存在'"
