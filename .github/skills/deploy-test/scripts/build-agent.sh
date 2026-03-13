#!/usr/bin/env bash
# build-agent.sh — 使用 scripts/build-and-package.sh 编译、打包并安装本机 agent
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/env.sh"

echo "==> 调用 scripts/build-and-package.sh --server ${REMOTE_HOST} --port ${SIGNAL_PORT} --room ${ROOM_ID} --install"
echo "${LOCAL_SUDO_PASS}" | sudo -S echo "sudo 已就绪" 2>/dev/null

bash "${REPO_ROOT}/scripts/build-and-package.sh" \
    --server "${REMOTE_HOST}" \
    --port "${SIGNAL_PORT}" \
    --room "${ROOM_ID}" \
    --install

echo ""
echo "==> 安装完成，检查服务状态："
sudo systemctl status any-kvm-agent --no-pager -l || true
