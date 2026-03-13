#!/usr/bin/env bash
# run-agent.sh — 通过 systemd 管理本机 agent 服务
#
# 前置条件：已通过 build-agent.sh（即 scripts/build-and-package.sh --install）安装
# 本脚本负责：检查安装→重启服务→跟踪日志
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/env.sh"

echo "==> [1/3] 检查 agent 安装"
if ! command -v any-kvm-agent &>/dev/null && [ ! -f /usr/bin/any-kvm-agent ]; then
    echo "  agent 未安装，先执行编译安装…"
    bash "${SCRIPT_DIR}/build-agent.sh"
fi

echo "  binary: $(which any-kvm-agent 2>/dev/null || echo /usr/bin/any-kvm-agent)"

echo "==> [2/3] 确认配置文件"
CONFIG="/etc/any-kvm-agent/config.toml"
if [ -f "${CONFIG}" ]; then
    echo "  配置文件: ${CONFIG}"
    grep -E '^\s*(url|room_id|source)\s*=' "${CONFIG}" 2>/dev/null || true
else
    echo "  ⚠️ 配置文件不存在，重新安装…"
    bash "${SCRIPT_DIR}/build-agent.sh"
fi

echo "==> [3/3] 重启 any-kvm-agent 服务"
echo "${LOCAL_SUDO_PASS}" | sudo -S systemctl restart any-kvm-agent 2>/dev/null
sleep 2
sudo systemctl status any-kvm-agent --no-pager -l

echo ""
echo "✅ Agent 已通过 systemd 启动"
echo "   查看日志: journalctl -u any-kvm-agent -f"
echo "   停止服务: sudo systemctl stop any-kvm-agent"
