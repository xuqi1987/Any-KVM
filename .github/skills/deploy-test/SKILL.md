---
name: deploy-test
description: "Any-KVM 部署测试验证工作流。使用时机: 部署信令服务器到远端(47.86.7.158)、本机运行 agent 做端到端测试、验证 WebRTC 连通性、检查健康状态、排查连接失败。包含: 构建 agent、部署 signal-server、配置并启动 agent、验证整个链路。"
argument-hint: "可选: deploy(仅部署服务端) | agent(仅启动agent) | verify(仅验证) | all(全流程)"
---

# Any-KVM 部署测试验证

## 环境信息

| 组件 | 地址 | 说明 |
|------|------|------|
| 信令服务器 | 47.86.7.158:8080 | 远端，Docker 运行 |
| coturn TURN | 47.86.7.158:3478 | 远端，Docker 运行 |
| Agent | 本机 | cargo release 构建后直接运行 |
| Web 控制台 | http://47.86.7.158:8080 | 静态文件由 signal 服务提供 |

凭据存放在 [scripts/env.sh](./scripts/env.sh)（已加入 .gitignore）。

---

## 完整流程（all）

按步骤执行，每步确认成功后再继续：

### Step 1 — 构建 Agent（本机）

```bash
bash .github/skills/deploy-test/scripts/build-agent.sh
```

### Step 2 — 部署信令服务器（远端）

```bash
bash .github/skills/deploy-test/scripts/deploy-signal.sh
```

### Step 3 — 准备本机 config.toml 并启动 Agent

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
| 仅重启远端服务 | `bash .github/skills/deploy-test/scripts/deploy-signal.sh` |
| 查看远端容器日志 | `bash .github/skills/deploy-test/scripts/remote-logs.sh` |
| 停止远端服务 | `bash .github/skills/deploy-test/scripts/stop-signal.sh` |
| 仅验证链路 | `bash .github/skills/deploy-test/scripts/verify.sh` |

---

## 故障排查

- **健康检查失败**: 检查 `remote-logs.sh` 中 signal 容器日志，确认端口 8080 未被占用
- **agent 连不上信令**: 确认 `config.toml` 中 `signal.url` 为 `ws://47.86.7.158:8080/ws`
- **WebRTC 无视频**: 检查浏览器控制台 ICE 连接状态；若均为 relay，确认 coturn 3478 端口开放
- **屏幕截图黑屏**: 确认 `DISPLAY` 环境变量已设置，或尝试 `DISPLAY=:0 ./any-kvm-agent config.toml`
- **v4l2 设备不存在**: agent 会自动回退到屏幕截图，无需手动操作
