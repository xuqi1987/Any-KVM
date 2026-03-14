# Any-KVM 工作区指令

Any-KVM 是一个**浏览器直访物理设备**的轻量级 KVM 控制平台。WebRTC P2P 直连传输视频/音频，服务器**只做信令交换**，不转发任何媒体数据。

---

## 架构组件

| 目录       | 语言             | 职责                                                                      |
| ---------- | ---------------- | ------------------------------------------------------------------------- |
| `agent/`   | Rust 2021        | 设备端：V4L2 视频采集、ALSA 音频、USB HID Gadget / CH9329、WebRTC (str0m) |
| `signal/`  | Go 1.21          | 信令服务器：gorilla/websocket，SDP/ICE 交换，无媒体转发，/api/agents 列表 |
| `web/`     | 原生 JS/HTML/CSS | 浏览器控制台：自动发现在线 Agent、RTCPeerConnection、视频渲染、HID 事件捕获 |
| `deploy/`  | Docker Compose   | coturn TURN + signal-server 容器编排                                      |
| `scripts/` | Bash             | 设备端一次性配置（USB HID Gadget）、一键编译打包脚本                       |

数据流：device → (WebRTC P2P) → browser；仅降级时经 TURN 中继。

---

## 典型工作流

### 1. 部署信令服务器（公网机器，一键部署）

```bash
# 修改 .github/skills/deploy-test/scripts/env.sh 中的 REMOTE_HOST / REMOTE_USER / REMOTE_PASS
# 然后执行一键部署（本地构建 + scp + 远端自动配置）
bash .github/skills/deploy-test/scripts/deploy-signal.sh
# 信令服务器监听 :8080，coturn 监听 :3478
curl http://<YOUR_SERVER_IP>:8080/health   # → {"status":"ok","rooms":0}
```

### 2. 在被控设备上编译安装 Agent（每台设备一次）

```bash
# 克隆项目后，在设备上执行
bash scripts/build-and-package.sh --install

# 或仅生成安装包后手动安装
bash scripts/build-and-package.sh
sudo dpkg -i dist/any-kvm-agent_0.1.0_arm64.deb   # Debian/Ubuntu/树莓派OS
# 通用 Linux：
# tar xzf dist/any-kvm-agent_0.1.0_arm64.tar.gz && sudo ./any-kvm-agent/install.sh
```

### 3. 配置 Agent（填写服务器地址和房间 ID）

```bash
sudo nano /etc/any-kvm-agent/config.toml
```

最少需要修改：

```toml
[signal]
url     = "ws://<YOUR_SERVER_IP>:8080/ws"   # 信令服务器公网地址
room_id = "my-device"                   # 任意唯一名称

[hid]
mode = "gadget"           # USB OTG 设备用 gadget；外挂芯片用 ch9329
```

然后启动：

```bash
sudo systemctl start any-kvm-agent
sudo systemctl status any-kvm-agent   # 确认 active (running)
```

### 4. 浏览器访问

打开 `http://<YOUR_SERVER_IP>:8080`（或域名 `http://xu7-kvm.xyz:8080`），页面自动显示在线设备列表，点击设备名称的「连接」按钮即可。
STUN/TURN/信令地址均从浏览器 URL 自动推导，无需手动填写。

---

## 信令服务器 API

| 路径           | 方法 | 说明                                                               |
| -------------- | ---- | ------------------------------------------------------------------ |
| `/ws`          | WS   | WebSocket 信令端点，参数 `?room=<id>&role=device\|client&name=<设备名>` |
| `/health`      | GET  | 健康检查，返回 `{"status":"ok","rooms":N}`                         |
| `/api/agents`  | GET  | 返回当前在线的 Agent 列表（有 device 连接的 room）                 |
| `/`            | GET  | 静态文件服务（Web 控制台）                                         |

`/api/agents` 响应格式：

```json
{
  "agents": [
    { "room_id": "my-device", "name": "my-device", "connected_at": "2026-03-14T10:00:00Z" }
  ]
}
```

---

## 构建命令

### Agent (Rust) — 一键编译打包（推荐，在目标设备上运行）

```bash
bash scripts/build-and-package.sh          # 生成 dist/ 下的 .deb 和 .tar.gz
bash scripts/build-and-package.sh --install  # 编译后直接安装到本机
```

### Agent (Rust) — 交叉编译（开发机，需 Docker Desktop 运行中）

```bash
cd agent

# 玩客云 / 树莓派 5 (aarch64)
cross build --target aarch64-unknown-linux-gnu --release

# Ubuntu 22.04 x86_64
cross build --target x86_64-unknown-linux-gnu --release
```

> **必要环境变量**（pkg-config 找不到依赖时）：
>
> ```bash
> PKG_CONFIG_LIBDIR=/usr/lib/aarch64-linux-gnu/pkgconfig:/usr/share/pkgconfig \
> PKG_CONFIG_ALLOW_CROSS=1 cross build ...
> ```

### Agent — 本机编译（在目标 Ubuntu 上）

```bash
cd agent
cargo build --release
# 输出：target/release/any-kvm-agent
```

### Signal Server (Go)

```bash
cd signal
go build -o signal-server .
go test ./...
```

### 服务器部署

```bash
cd deploy
docker compose up -d
curl http://localhost:8080/health   # → {"status":"ok"}
```

---

## 关键依赖与版本约束

- **str0m `0.5`** — Pure Rust WebRTC，API 注意事项：
  - `Rtc::builder().build()` — 无 `add_ice_server()` 方法，ICE server 暂不通过 builder 配置
  - 本地候选用 `rtc.add_local_candidate(Candidate::host(addr, Protocol::Udp)?)`
  - 网络接收用 `Receive::new(Protocol::Udp, src, dst, &buf[..n])` 而非直接构造 struct
  - `Event` 无 `IceCandidate` 变体；本地候选需手动注册，远端候选用 `rtc.add_remote_candidate()`
  - 媒体写入用 `rtc.direct_api().stream_tx_by_mid(mid, None)` 获取 writer
- **audiopus `0.3.0-rc.0`** — crates.io 上没有稳定的 `0.3`，必须用 rc 版本
- **serialport** — 通过 `ch9329` feature flag 设为可选依赖（避免 `libudev` 在 aarch64 交叉编译报错）
- **cross** — 从 git 安装：`cargo install cross --git https://github.com/cross-rs/cross`

---

## 项目约定

### Rust Agent

- 每个模块对应单一职责：`video.rs` / `audio.rs` / `hid.rs` / `webrtc.rs` / `signal_client.rs`
- 模块间通信全部走 `tokio::sync::mpsc`（媒体帧、HID 控制帧）或 `oneshot`（SDP offer）
- 错误处理用 `anyhow`，日志用 `tracing`（结构化，`RUST_LOG=any_kvm_agent=debug` 控制级别）
- HID 串口支持通过 `#[cfg(feature = "ch9329")]` 条件编译隔离，默认不编译
- Agent 连接信令时可传 `?name=<设备名>` 参数，Web 界面会显示该名称

### Config

- 设备端配置文件：`agent/config.toml`（参考 `config.toml.example`）
- 最少必填项：`signal.url`、`signal.room_id`
- `signal.url` 末尾不含 `/ws` 路径；agent 自动在内部拼接

### Web 前端

- 无构建步骤，纯静态文件，由 signal-server 通过 HTTP 静态伺服直接提供
- 连接流程：输入 `ws://server:8080/ws` → 自动拉取 `/api/agents` → 点击设备卡片连接
- STUN 服务器内置（含国内友好节点），用户无需手动填写
- 服务器地址记忆在 localStorage，刷新页面后自动恢复

---

## 常见陷阱

1. **cross build 失败找不到 pkg-config** — 设置 `PKG_CONFIG_LIBDIR` 和 `PKG_CONFIG_ALLOW_CROSS=1`
2. **Docker 未运行** — cross 依赖 Docker；编译前确认 `docker info` 无报错
3. **`~/.cargo/bin` 不在 PATH** — rustup 安装后需手动 `source ~/.zshrc` 或重开终端
4. **str0m API 破坏性变更** — str0m 0.4 → 0.5 有重大变更；`Receive`、`DatagramRecv`、`Event` 枚举均有改动，参考 `~/.cargo/registry/src/*/str0m-0.5.*/src/` 查阅实际 API
5. **`[features]` 位置** — Cargo.toml 中 `[features]` 必须放在 `[dependencies]` 块**之后**，否则解析错误
6. **`/api/agents` 无 CORS 问题** — 该接口已设置 `Access-Control-Allow-Origin: *`，Web 页面跨域调用正常

---

## 关键文件速览

| 文件                            | 作用                                                                     |
| ------------------------------- | ------------------------------------------------------------------------ |
| `agent/src/webrtc.rs`           | str0m 引擎：ICE + DTLS + SRTP，SDP offer/answer，RTP 发送                |
| `agent/src/signal_client.rs`    | WebSocket 信令客户端，SDP/ICE 消息路由                                   |
| `agent/Cross.toml`              | cross 交叉编译：pre-build 安装系统库，pkg-config 环境透传                |
| `signal/main.go`                | 信令服务器：WebSocket hub，SDP/ICE 转发，/health，/api/agents，静态文件  |
| `web/app.js`                    | 浏览器端：Agent 发现列表、WebRTC 控制逻辑、HID DataChannel 消息发送      |
| `scripts/build-and-package.sh`  | 一键编译+打包：生成 .deb 和 .tar.gz，支持 --install 直接安装             |
| `scripts/any-kvm-agent.service` | systemd 服务单元（开机自启，打包时自动嵌入）                             |
| `deploy/docker-compose.yml`     | signal-server + coturn 容器编排，端口映射                                |
| `docs/02-architecture.md`       | 系统架构决策文档                                                         |

---

## 构建命令

### Agent (Rust) — 交叉编译（开发机，需 Docker Desktop 运行中）
