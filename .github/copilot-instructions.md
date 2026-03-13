# Any-KVM 工作区指令

Any-KVM 是一个**浏览器直访物理设备**的轻量级 KVM 控制平台。WebRTC P2P 直连传输视频/音频，服务器**只做信令交换**，不转发任何媒体数据。

---

## 架构组件

| 目录       | 语言             | 职责                                                                      |
| ---------- | ---------------- | ------------------------------------------------------------------------- |
| `agent/`   | Rust 2021        | 设备端：V4L2 视频采集、ALSA 音频、USB HID Gadget / CH9329、WebRTC (str0m) |
| `signal/`  | Go 1.21          | 信令服务器：gorilla/websocket，SDP/ICE 交换，无媒体转发                   |
| `web/`     | 原生 JS/HTML/CSS | 浏览器控制台：RTCPeerConnection、视频渲染、HID 事件捕获                   |
| `deploy/`  | Docker Compose   | coturn TURN + signal-server 容器编排                                      |
| `scripts/` | Bash             | 设备端一次性配置（USB HID Gadget）                                        |

数据流：device → (WebRTC P2P) → browser；仅降级时经 TURN 中继。

---

## 构建命令

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

### Config

- 设备端配置文件：`agent/config.toml`（参考 `config.toml.example`）
- 最少必填项：`signal.url`、`signal.room_id`、`ice.turn_*`

### Web 前端

- 无构建步骤，纯静态文件，由 signal-server 通过 HTTP 静态伺服直接提供
- WebRTC 信令通过 WebSocket 连接 `ws://<server>/ws?room=<id>&role=browser`

---

## 常见陷阱

1. **cross build 失败找不到 pkg-config** — 设置 `PKG_CONFIG_LIBDIR` 和 `PKG_CONFIG_ALLOW_CROSS=1`
2. **Docker 未运行** — cross 依赖 Docker；编译前确认 `docker info` 无报错
3. **`~/.cargo/bin` 不在 PATH** — rustup 安装后需手动 `source ~/.zshrc` 或重开终端
4. **str0m API 破坏性变更** — str0m 0.4 → 0.5 有重大变更；`Receive`、`DatagramRecv`、`Event` 枚举均有改动，参考 `~/.cargo/registry/src/*/str0m-0.5.*/src/` 查阅实际 API
5. **`[features]` 位置** — Cargo.toml 中 `[features]` 必须放在 `[dependencies]` 块**之后**，否则解析错误

---

## 关键文件速览

| 文件                         | 作用                                                           |
| ---------------------------- | -------------------------------------------------------------- |
| `agent/src/webrtc.rs`        | str0m 引擎：ICE + DTLS + SRTP，SDP offer/answer，RTP 发送      |
| `agent/src/signal_client.rs` | WebSocket 信令客户端，SDP/ICE 消息路由                         |
| `agent/Cross.toml`           | cross 交叉编译：pre-build 安装系统库，pkg-config 环境透传      |
| `signal/main.go`             | 信令服务器：WebSocket hub，SDP/ICE 转发，/health，静态文件服务 |
| `web/app.js`                 | 浏览器端 WebRTC 控制逻辑、HID DataChannel 消息发送             |
| `deploy/docker-compose.yml`  | signal-server + coturn 容器编排，端口映射                      |
| `docs/02-architecture.md`    | 系统架构决策文档                                               |
