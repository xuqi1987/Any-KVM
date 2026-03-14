---
name: deploy-test
description: "Any-KVM 部署测试验证工作流。使用时机: 部署信令服务器到远端(47.86.7.158)、本机编译安装 agent 做端到端测试、验证 WebRTC 连通性、检查健康状态、排查连接失败。包含: 构建打包安装 agent、部署 signal-server、管理 systemd 服务、验证整个链路。"
argument-hint: "可选: deploy(仅部署服务端) | agent(仅编译安装agent) | verify(仅验证) | all(全流程)"
---

# Any-KVM 部署测试验证

## 环境信息

| 组件 | 地址 | 说明 |
|------|------|------|
| 信令服务器 | 47.86.7.158:8080 | 远端，Docker 运行（参照 auto-deploy.yml） |
| coturn TURN | 47.86.7.158:3478 | 远端，Docker 运行 |
| Agent | 本机 | `scripts/build-and-package.sh --install` 编译安装，systemd 管理 |
| Web 控制台 | http://47.86.7.158:8080 | 静态文件由 signal 服务提供 |

凭据存放在 [scripts/env.sh](./scripts/env.sh)（已加入 .gitignore）。

---

## 完整流程（all）

按步骤执行，每步确认成功后再继续：

### Step 1 — 编译打包并安装 Agent（本机）

调用 `scripts/build-and-package.sh --install`，自动完成编译、打包、安装到系统、启动 systemd 服务。

```bash
bash .github/skills/deploy-test/scripts/build-agent.sh
```

### Step 2 — 部署信令服务器（远端）

通过 scp 上传本地文件到远端服务器，然后 docker-compose 构建启动。无需远端访问 GitHub。

```bash
bash .github/skills/deploy-test/scripts/deploy-signal.sh
```

### Step 3 — 启动/重启 Agent 服务

通过 systemd 管理 agent 服务（由 Step 1 安装）。

```bash
bash .github/skills/deploy-test/scripts/run-agent.sh
```

### Step 4 — 验证整体连通性

```bash
bash .github/skills/deploy-test/scripts/verify.sh
```

---

## 单项操作

| 目标 | 命令 |
|------|------|
| 仅编译安装 agent | `bash .github/skills/deploy-test/scripts/build-agent.sh` |
| 仅重启远端服务 | `bash .github/skills/deploy-test/scripts/deploy-signal.sh` |
| 重启本机 agent | `bash .github/skills/deploy-test/scripts/run-agent.sh` |
| 查看远端容器日志 | `bash .github/skills/deploy-test/scripts/remote-logs.sh` |
| 停止远端服务 | `bash .github/skills/deploy-test/scripts/stop-signal.sh` |
| 仅验证链路 | `bash .github/skills/deploy-test/scripts/verify.sh` |

---

## Agent 安装机制说明

- 编译打包统一使用 `scripts/build-and-package.sh`（生成 .deb + .tar.gz）
- `--install` 参数会自动 `dpkg -i` 安装到系统并启动 systemd 服务
- systemd 服务单元 `any-kvm-agent.service` 使用 wrapper 脚本自动启动 Xvfb（屏幕截图源需要）
- 配置文件存放于 `/etc/any-kvm-agent/config.toml`
- 日志查看：`journalctl -u any-kvm-agent -f`

## Web 服务器部署说明

远端部署流程（通过 scp 上传，不依赖 GitHub）：
1. SSH 到远端 → 检查/安装 docker + docker-compose
2. `scp` 上传本地 signal/、deploy/、web/ 目录到远端 `/root/Any-KVM/`
3. 配置 `coturn.conf`（替换公网 IP 和 TURN 密码）
4. `docker-compose up -d --build`（构建 signal-server + 启动 coturn）
5. 健康检查 `/health` 确认就绪

---

## 故障排查

- **健康检查失败**: `bash .github/skills/deploy-test/scripts/remote-logs.sh` 查看容器日志
- **agent 连不上信令**: 检查 `/etc/any-kvm-agent/config.toml` 中 `signal.url`
- **WebRTC 无视频**: 检查 `journalctl -u any-kvm-agent -f` 中是否有 `screen capture` 字样
- **屏幕截图黑屏**: wrapper 脚本会自动启 Xvfb，确认服务使用 wrapper 启动
- **v4l2 设备不存在**: agent 会自动回退到屏幕截图
