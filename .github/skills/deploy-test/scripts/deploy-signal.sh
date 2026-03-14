#!/usr/bin/env bash
# deploy-signal.sh — 一键部署 signal-server + coturn（无需 Docker）
# 只需修改 env.sh 中的 REMOTE_HOST / REMOTE_USER / REMOTE_PASS，其余全自动。
# 流程：本地构建 Go 二进制 → scp 上传 → apt 装 coturn → 自动配置 IP → systemd 启动
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/env.sh"

REMOTE_DIR="/root/Any-KVM"
SIGNAL_BIN="${REPO_ROOT}/signal/signal-server"

# ─── Step 0: 本地构建 signal-server 二进制 ──────────────────────────────────
echo "==> [0/4] 本地构建 signal-server 二进制"
if [[ -f "${SIGNAL_BIN}" ]]; then
    echo "✓ 二进制已存在: $(ls -lh "${SIGNAL_BIN}" | awk '{print $5}')"
    echo "  (如需重新构建，请先删除 ${SIGNAL_BIN})"
else
    echo "→ 使用 Docker 构建 signal-server（静态链接，linux/amd64）..."
    docker run --rm -v "${REPO_ROOT}/signal":/src -w /src golang:1.21-alpine sh -c \
        "go env -w GOPROXY=https://goproxy.cn,direct && CGO_ENABLED=0 GOOS=linux go build -ldflags='-s -w' -o /src/signal-server ."
    echo "✓ 构建完成: $(ls -lh "${SIGNAL_BIN}" | awk '{print $5}')"
fi

# ─── Step 1: 远端安装 coturn ─────────────────────────────────────────────────
echo "==> [1/4] 远端安装依赖（coturn）"
ssh_remote 'bash -s' << 'INSTALL_DEPS'
set -e
export DEBIAN_FRONTEND=noninteractive
if ! command -v turnserver &> /dev/null; then
    echo "→ 安装 coturn..."
    apt-get update -qq
    apt-get install -y -o Dpkg::Options::="--force-confdef" -o Dpkg::Options::="--force-confold" coturn
    echo "✓ coturn 已安装"
else
    echo "✓ coturn 已存在: $(turnserver --version 2>&1 | head -1)"
fi
# 确保 curl 可用
command -v curl &> /dev/null || apt-get install -y curl
INSTALL_DEPS

# ─── Step 2: scp 上传文件 ───────────────────────────────────────────────────
echo "==> [2/4] 上传文件到远端"
ssh_remote "mkdir -p ${REMOTE_DIR}/web"

# 上传 signal-server 二进制
echo "  → signal-server 二进制..."
scp_to_remote "${SIGNAL_BIN}" "${REMOTE_DIR}/signal-server"
ssh_remote "chmod +x ${REMOTE_DIR}/signal-server"

# 上传 coturn 配置模板
echo "  → coturn.conf..."
scp_to_remote "${REPO_ROOT}/deploy/coturn.conf" "${REMOTE_DIR}/coturn.conf"

# 上传 Web 前端（地址从 window.location 自动推导，无需替换）
echo "  → Web 静态文件..."
scp_to_remote "${REPO_ROOT}/web/index.html" "${REMOTE_DIR}/web/index.html"
scp_to_remote "${REPO_ROOT}/web/app.js" "${REMOTE_DIR}/web/app.js"
scp_to_remote "${REPO_ROOT}/web/style.css" "${REMOTE_DIR}/web/style.css"

echo "✓ 文件上传完成"

# ─── Step 3: 远端配置并启动服务 ─────────────────────────────────────────────
echo "==> [3/4] 远端配置并启动服务"
# 将 REMOTE_HOST 传给远端脚本（作为 heredoc 变量注入）
ssh_remote "DEPLOY_PUBLIC_IP=${REMOTE_HOST}" 'bash -s' << 'DEPLOY'
set -e
REMOTE_DIR="/root/Any-KVM"

# --- 停止旧服务 ---
systemctl stop any-kvm-signal 2>/dev/null || true
systemctl stop coturn 2>/dev/null || true
docker rm -f any-kvm-signal any-kvm-coturn 2>/dev/null || true

# --- 配置 coturn：自动填充公网 IP 和内网 IP ---
PRIVATE_IP=$(hostname -I | awk '{print $1}')
echo "  公网 IP: ${DEPLOY_PUBLIC_IP}, 内网 IP: ${PRIVATE_IP}"

cp "${REMOTE_DIR}/coturn.conf" /etc/turnserver.conf
sed -i "s|__PUBLIC_IP__|${DEPLOY_PUBLIC_IP}|g" /etc/turnserver.conf
sed -i "s|__PRIVATE_IP__|${PRIVATE_IP}|g" /etc/turnserver.conf

# 启用 coturn 服务
echo 'TURNSERVER_ENABLED=1' > /etc/default/coturn
systemctl enable coturn
systemctl restart coturn
echo "✓ coturn 已启动 (external-ip=${DEPLOY_PUBLIC_IP}/${PRIVATE_IP})"

# --- 创建 signal-server systemd 服务 ---
cat > /etc/systemd/system/any-kvm-signal.service << 'SERVICE'
[Unit]
Description=Any-KVM Signal Server
After=network.target

[Service]
Type=simple
ExecStart=/root/Any-KVM/signal-server -addr :8080 -web /root/Any-KVM/web
WorkingDirectory=/root/Any-KVM
Restart=always
RestartSec=3
Environment=TZ=Asia/Shanghai

[Install]
WantedBy=multi-user.target
SERVICE

systemctl daemon-reload
systemctl enable any-kvm-signal
systemctl restart any-kvm-signal
echo "✓ signal-server 已启动"

sleep 2

# --- 健康检查 ---
if curl -s --max-time 5 http://localhost:8080/health | grep -q "ok"; then
    echo "✓ 信令服务器运行正常"
    curl -s http://localhost:8080/health
else
    echo "⚠ 信令服务器可能未就绪，查看日志："
    journalctl -u any-kvm-signal --no-pager -n 20
fi

echo ""
echo "--- 服务状态 ---"
systemctl is-active any-kvm-signal && echo "  signal-server: running" || echo "  signal-server: NOT running"
systemctl is-active coturn && echo "  coturn: running" || echo "  coturn: NOT running"
DEPLOY

# ─── Step 4: 本地验证 ──────────────────────────────────────────────────────
echo "==> [4/4] 本地验证远端服务"
for i in $(seq 1 10); do
    STATUS=$(curl -sf "http://${REMOTE_HOST}:${SIGNAL_PORT}/health" 2>/dev/null || echo 'NOT_READY')
    if echo "${STATUS}" | grep -q '"status":"ok"'; then
        echo "✅ 信令服务器已就绪: http://${REMOTE_HOST}:${SIGNAL_PORT}/health"
        echo "   ${STATUS}"
        exit 0
    fi
    echo "   等待中… (${i}/10)"
    sleep 3
done
echo "❌ 服务未在 30 秒内就绪"
exit 1
