#!/usr/bin/env bash
# deploy-signal.sh — 将最新代码推送到远端，并重建启动 signal-server + coturn
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/env.sh"

REMOTE_REPO="/opt/any-kvm"

echo "==> [1/4] 检查远端依赖（git、docker）"
ssh_remote "command -v git docker >/dev/null 2>&1 || (apt-get update -qq && apt-get install -y -qq git docker.io docker-compose-plugin)"

echo "==> [2/4] 同步仓库到远端 ${REMOTE_REPO}"
ssh_remote "
  if [ -d '${REMOTE_REPO}/.git' ]; then
    cd ${REMOTE_REPO} && git fetch origin && git reset --hard origin/main
  else
    git clone https://github.com/$(git -C '${REPO_ROOT}' remote get-url origin 2>/dev/null | sed 's|.*github.com[:/]||;s|\.git$||' || echo 'your-org/Any-KVM') ${REMOTE_REPO} 2>/dev/null \
      || { mkdir -p ${REMOTE_REPO}; echo 'clone skipped, using rsync'; }
  fi
"

# 若 git clone 不可用则用 rsync 同步关键目录
echo "==> [2b/4] rsync 同步 signal/ web/ deploy/ 到远端"
sshpass -p "${REMOTE_PASS}" rsync -az --delete \
  -e "ssh -o StrictHostKeyChecking=no" \
  "${REPO_ROOT}/signal/" \
  "${REPO_ROOT}/web/" \
  "${REPO_ROOT}/deploy/" \
  "${REMOTE_USER}@${REMOTE_HOST}:${REMOTE_REPO}/" 2>/dev/null || true

echo "==> [3/4] 远端 docker compose 构建并启动"
ssh_remote "
  cd ${REMOTE_REPO}
  docker compose -f deploy/docker-compose.yml down --remove-orphans 2>/dev/null || true
  docker compose -f deploy/docker-compose.yml up -d --build
"

echo "==> [4/4] 等待服务就绪"
for i in $(seq 1 15); do
    STATUS=$(sshpass -p "${REMOTE_PASS}" ssh -o StrictHostKeyChecking=no \
        "${REMOTE_USER}@${REMOTE_HOST}" \
        "curl -sf http://localhost:${SIGNAL_PORT}/health 2>/dev/null || echo 'NOT_READY'" 2>/dev/null || echo 'NOT_READY')
    if echo "${STATUS}" | grep -q '"status":"ok"'; then
        echo "✅ 信令服务器已就绪: http://${REMOTE_HOST}:${SIGNAL_PORT}/health"
        echo "   ${STATUS}"
        exit 0
    fi
    echo "   等待中… (${i}/15)"
    sleep 2
done
echo "❌ 服务未在 30 秒内就绪，请检查日志: bash remote-logs.sh"
exit 1
