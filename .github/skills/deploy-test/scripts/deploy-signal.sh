#!/usr/bin/env bash
# deploy-signal.sh — 参照 auto-deploy.yml 流程，在远端部署 signal-server + coturn
# 流程：git clone/pull → 配置 coturn → docker-compose up
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/env.sh"

echo "==> [1/4] 远端检查依赖（docker、docker-compose、git）"
ssh_remote 'bash -s' << 'CHECK_DEPS'
set -e
if ! command -v docker &> /dev/null; then
    echo "→ 安装 Docker..."
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y -o Dpkg::Options::="--force-confdef" -o Dpkg::Options::="--force-confold" \
        docker.io curl git wget || {
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

echo "==> [2/4] 远端 git clone/pull 代码"
ssh_remote 'bash -s' << 'GIT_SYNC'
set -e
cd /root
if [ -d "Any-KVM" ]; then
    cd Any-KVM && git pull && cd /root
    echo "✓ 代码已更新"
else
    for i in 1 2 3; do
        if timeout 300 git clone --depth 1 https://github.com/xuqi1987/Any-KVM.git; then
            echo "✓ 代码已克隆"
            break
        fi
        echo "⚠ 克隆失败，重试 ($i/3)..."
        sleep 5
    done
fi
[ -d "Any-KVM" ] || { echo "❌ 代码获取失败"; exit 1; }
GIT_SYNC

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
