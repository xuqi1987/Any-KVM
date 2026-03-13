#!/usr/bin/env bash
# verify.sh — 端到端验证: 信令服务、agent 在线、Web 可访问
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/env.sh"

PASS=0
FAIL=0

check() {
    local desc="$1"
    local cmd="$2"
    if eval "${cmd}" >/dev/null 2>&1; then
        echo "  ✅ ${desc}"
        ((PASS++)) || true
    else
        echo "  ❌ ${desc}"
        ((FAIL++)) || true
    fi
}

echo "========================================"
echo " Any-KVM 端到端验证"
echo " 信令: ${WEB_URL}"
echo "========================================"

echo ""
echo "── 远端服务 ──────────────────────────────"
check "信令服务器健康检查" \
    "curl -sf '${WEB_URL}/health' | grep -q '\"status\":\"ok\"'"

check "Web 控制台可访问 (HTTP 200)" \
    "curl -sf -o /dev/null -w '%{http_code}' '${WEB_URL}/' | grep -q '200'"

check "/api/agents 接口可访问" \
    "curl -sf '${WEB_URL}/api/agents'"

echo ""
echo "── Agent 在线状态 ────────────────────────"
AGENTS_RESP=$(curl -sf "${WEB_URL}/api/agents" 2>/dev/null || echo '{"agents":[]}')
AGENT_COUNT=$(echo "${AGENTS_RESP}" | grep -o '"room_id"' | wc -l)

if [ "${AGENT_COUNT}" -gt 0 ]; then
    echo "  ✅ 在线 Agent 数量: ${AGENT_COUNT}"
    echo "    详情: ${AGENTS_RESP}"
    ((PASS++)) || true
else
    echo "  ⚠️  暂无 Agent 在线 (room_id count=0)"
    echo "     请先运行: bash run-agent.sh"
    echo "     响应内容: ${AGENTS_RESP}"
fi

echo ""
echo "── 本机 Agent 进程 ───────────────────────"
check "any-kvm-agent 进程运行中" \
    "pgrep -x any-kvm-agent"

echo ""
echo "── 网络连通性 ────────────────────────────"
check "远端 8080 TCP 可达" \
    "timeout 3 bash -c 'echo > /dev/tcp/${REMOTE_HOST}/${SIGNAL_PORT}'"
check "远端 3478 UDP 可达 (STUN)" \
    "timeout 3 bash -c 'echo > /dev/tcp/${REMOTE_HOST}/3478'" || true  # tcp 探测 turn 端口

echo ""
echo "── sshpass 远端容器状态 ──────────────────"
CONTAINERS=$(ssh_remote "docker ps --format 'table {{.Names}}\t{{.Status}}' 2>/dev/null" 2>/dev/null || echo "SSH 失败")
echo "  ${CONTAINERS}"

echo ""
echo "========================================"
echo " 结果: ✅ ${PASS} 通过 / ❌ ${FAIL} 失败"
echo "========================================"

if [ "${FAIL}" -gt 0 ]; then
    echo ""
    echo "排查提示:"
    echo "  - 查看服务器日志: bash remote-logs.sh"
    echo "  - 重新部署服务端: bash deploy-signal.sh"
    echo "  - 启动本地 agent: bash run-agent.sh"
    exit 1
fi
