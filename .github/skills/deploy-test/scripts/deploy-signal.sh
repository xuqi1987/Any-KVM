#!/usr/bin/env bash
# deploy-signal.sh — 通过 scp 上传本地文件到远端，部署 signal-server + coturn
# 流程：scp 上传必要文件 → 配置 coturn → docker build + docker-compose up
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/env.sh"

REMOTE_DIR="/root/Any-KVM"

echo "==> [1/4] 远端检查依赖（docker、docker-compose）"
ssh_remote 'bash -s' << 'CHECK_DEPS'
set -e
if ! command -v docker &> /dev/null; then
    echo "→ 安装 Docker..."
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y -o Dpkg::Options::="--force-confdef" -o Dpkg::Options::="--force-confold" \
        docker.io curl wget || {
        curl -fsSL https://get.docker.com | sh
    }
    systemctl start docker || true
fi
if ! command -v docker-compose &> /dev/null; then
    echo "→ 安装 docker-compose..."
    apt-get install -y docker-compose 2>/dev/null || {
        curl -fsSL -o /usr/local/bin/docker-compose \
            "https://github.com/docker/compose/releases/download/v2.24.0/docker-compose-$(uname -s)-$(uname -m)"
        chmod +x /usr/local/bin/docker-compose
    }
fi
echo "✓ docker $(docker --version | grep -oP '\d+\.\d+\.\d+')"
echo "✓ docker-compose $(docker-compose version --short 2>/dev/null || docker-compose --version)"
CHECK_DEPS

echo "==> [2/4] 通过 scp 上传项目文件到远端"
# 在远端创建目录结构
ssh_remote "mkdir -p ${REMOTE_DIR}/{signal,deploy,web}"

# 上传信令服务器源码
scp_to_remote "${REPO_ROOT}/signal/go.mod" "${REMOTE_DIR}/signal/go.mod"
scp_to_remote "${REPO_ROOT}/signal/go.sum" "${REMOTE_DIR}/signal/go.sum" 2>/dev/null || true
scp_to_remote "${REPO_ROOT}/signal/main.go" "${REMOTE_DIR}/signal/main.go"

# 上传部署配置
scp_to_remote "${REPO_ROOT}/deploy/docker-compose.yml" "${REMOTE_DIR}/deploy/docker-compose.yml"
scp_to_remote "${REPO_ROOT}/deploy/Dockerfile.signal" "${REMOTE_DIR}/deploy/Dockerfile.signal"
scp_to_remote "${REPO_ROOT}/deploy/coturn.conf" "${REMOTE_DIR}/deploy/coturn.conf"

# 上传 Web 前端静态文件
scp_to_remote "${REPO_ROOT}/web/index.html" "${REMOTE_DIR}/web/index.html"
scp_to_remote "${REPO_ROOT}/web/app.js" "${REMOTE_DIR}/web/app.js"
scp_to_remote "${REPO_ROOT}/web/style.css" "${REMOTE_DIR}/web/style.css"

echo "✓ 文件已上传到远端 ${REMOTE_DIR}"

echo "==> [3/4] 远端配置 & docker-compose 启动"
ssh_remote 'bash -s' << 'DEPLOY'
set -e
cd /root/Any-KVM/deploy

# 配置 coturn（替换公网 IP 和密码）
PUBLIC_IP=$(curl -s --max-time 10 ifconfig.me || curl -s --max-time 10 icanhazip.com || true)
if [ -z "$PUBLIC_IP" ]; then
    echo "⚠ 无法获取公网 IP，使用 hostname -I"
    PUBLIC_IP=$(hostname -I | awk '{print $1}')
fi

# 仅在还未替换时才 sed（避免重复部署时覆盖）
if grep -q "YOUR_PUBLIC_IP" coturn.conf 2>/dev/null; then
    TURN_PASSWORD=$(openssl rand -base64 24 | tr -d "=+/" | cut -c1-20)
    sed -i "s/YOUR_PUBLIC_IP/$PUBLIC_IP/g" coturn.conf
    sed -i "s/CHANGE_ME_IN_PRODUCTION/$TURN_PASSWORD/g" coturn.conf
    echo "✓ coturn.conf 已配置 (IP=$PUBLIC_IP)"
else
    echo "✓ coturn.conf 已有配置，跳过"
fi

# 停旧容器、启新容器
docker rm -f any-kvm-signal any-kvm-coturn 2>/dev/null || true
docker-compose down --remove-orphans 2>/dev/null || true
docker-compose up -d --build

sleep 5

# 健康检查
if curl -s --max-time 5 http://localhost:8080/health | grep -q "ok"; then
    echo "✓ 信令服务器运行正常"
else
    echo "⚠ 信令服务器可能未就绪"
fi
docker-compose ps
DEPLOY

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
