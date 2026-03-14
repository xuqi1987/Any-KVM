# Any-KVM 工作区指令

Any-KVM 是一个**浏览器直访物理设备**的轻量级 KVM 控制平台。WebRTC P2P 直连传输视频/音频，服务器**只做信令交换**，不转发任何媒体数据。

---

## 架构组件

| 目录       | 语言             | 职责                                                                      |
| ---------- | ---------------- | ------------------------------------------------------------------------- |
| `agent/`   | Rust 2021        | 设备端：V4L2 视频采集、ALSA 音频、USB HID Gadget / CH9329、WebRTC (str0m) |
| `signal/`  | Go 1.21          | 信令服务器：gorilla/websocket，SDP/ICE 交换，无媒体转发，/api/agents 列表 |
| `web/`     | 原生 JS/HTML/CSS | 浏览器控制台：自动发现在线 Agent、RTCPeerConnection、视频渲染、HID 事件捕获 |
| `deploy/`  | Docker Compose   | coturn TURN + signal-server 容器编排（仅用于信令/TURN，不传媒体）         |
| `scripts/` | Bash             | 设备端一次性配置（USB HID Gadget）、一键编译打包脚本                       |

数据流：device → (WebRTC P2P) → browser；仅降级时经 TURN 中继。

---

## 部署配置（单一配置点）

> **所有服务器 IP、密码均集中在一个文件里**，项目其他代码/文档中**不应出现**真实 IP。
>
> | 配置文件 | 说明 |
> |---|---|
> | `deploy/env.example` | 配置模板，列出所有可填变量（已纳入版本控制） |
> | `.github/skills/deploy-test/scripts/env.sh` | **真实配置**（已加入 `.gitignore`，不上传）|
>
> 首次使用：
> ```bash
> cp deploy/env.example .github/skills/deploy-test/scripts/env.sh
> # 编辑 env.sh，填入真实 REMOTE_HOST / REMOTE_PASS / TURN_PASSWORD
> ```
> GitHub Actions 通过 `secrets.SERVER_IP` + `secrets.SSH_PASSWORD` 配置，在仓库 Settings → Secrets 中设置。

以下为当前实际部署状态（以 `env.sh` 中 `REMOTE_HOST` 为准）：

| 资源                     | 地址/状态                                                    |
| ------------------------ | ------------------------------------------------------------ |
| 公网信令服务器           | `$REMOTE_HOST:$SIGNAL_PORT`，**运行中**                      |
| coturn TURN              | `$REMOTE_HOST:$TURN_PORT`，user `$TURN_USERNAME`             |
| 本机 Agent (xuqi-pc)     | `/usr/bin/any-kvm-agent`，systemd 服务，**在线**             |
| 玩客云 (192.168.31.132)  | armv7l Armbian 22.04，root/密码见 env.sh，**待部署**         |
| Web 静态文件（服务器端） | `/root/Any-KVM/web/`，由 signal-server `-web` 参数指向      |

---

## 典型工作流

### 1. 部署/更新信令服务器（公网机器，一键部署）

```bash
# 1. 确保 env.sh 已配置（首次使用：cp deploy/env.example .github/skills/deploy-test/scripts/env.sh）
# 2. 执行一键部署（本地构建 + scp + 远端自动配置）
bash .github/skills/deploy-test/scripts/deploy-signal.sh
# 信令服务器监听 :$SIGNAL_PORT，coturn 监听 :$TURN_PORT
source .github/skills/deploy-test/scripts/env.sh
curl http://${REMOTE_HOST}:${SIGNAL_PORT}/health   # → {"status":"ok","rooms":N}
```

### 2. 在 x86_64 设备上编译安装 Agent（本机或目标机）

```bash
# 在目标设备上克隆项目后执行（自动编译 + 打包 + 安装 + 启动 systemd 服务）
bash scripts/build-and-package.sh --install
# 服务二进制：/usr/bin/any-kvm-agent
# 启动包装脚本：/usr/bin/any-kvm-agent-wrapper（设置 DISPLAY=:99，启动 Xvfb）
```

### 3. 为 玩客云（armv7l）交叉编译 Agent

玩客云是 armv7l (armhf) 架构，使用自定义 Docker 镜像编译：

```bash
# 第一步：构建编译镜像（一次性）
cd agent
docker build -f Dockerfile.armv7 -t any-kvm-armv7-builder .

# 第二步：编译 armv7 二进制
docker run --rm -v "$(pwd)":/src any-kvm-armv7-builder
# 输出：agent/target/armv7-unknown-linux-gnueabihf/release/any-kvm-agent
```

### 4. 将 Agent 部署到玩客云

```bash
# 参考 .github/skills/deploy-test/ 中的 deploy-test skill 执行端到端部署
bash .github/skills/deploy-test/scripts/build-agent.sh   # 仅编译本机版本
# 玩客云手动部署：scp 二进制到 192.168.31.132，配置 /etc/any-kvm-agent/config.toml
```

### 5. 配置 Agent（填写服务器地址和房间 ID）

```bash
sudo nano /etc/any-kvm-agent/config.toml
```

最少需要修改：

```toml
[signal]
url     = "ws://<SIGNAL_HOST>:8080/ws"   # 填入 env.sh 中的 REMOTE_HOST
room_id = "my-device"                    # 任意唯一名称

[turn]
url      = "turn:<SIGNAL_HOST>:3478"     # 同上，通常与信令服务器同一台机器
username = "kvmuser"                     # 对应 env.sh 中的 TURN_USERNAME
password = "<TURN_PASSWORD>"             # 对应 env.sh 中的 TURN_PASSWORD

[hid]
mode = "gadget"           # USB OTG 设备用 gadget；外挂芯片用 ch9329
```

然后启动：

```bash
sudo systemctl start any-kvm-agent
sudo systemctl status any-kvm-agent   # 确认 active (running)
journalctl -u any-kvm-agent -f        # 实时查看日志
```

### 6. 浏览器访问

打开 `http://<SIGNAL_HOST>:8080`（`SIGNAL_HOST` = env.sh 中的 `REMOTE_HOST`），页面自动显示在线设备列表，点击设备名称的「连接」按钮即可。
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

### Agent (Rust) — armv7 交叉编译（玩客云，开发机上运行，需 Docker）

```bash
cd agent
# 如尚未构建镜像：
docker build -f Dockerfile.armv7 -t any-kvm-armv7-builder .
# 编译：
docker run --rm -v "$(pwd)":/src any-kvm-armv7-builder
# 输出：target/armv7-unknown-linux-gnueabihf/release/any-kvm-agent
```

### Agent (Rust) — aarch64 交叉编译（树莓派 5 等，需 cross 工具）

```bash
cd agent
# 安装 cross：cargo install cross --git https://github.com/cross-rs/cross
cross build --target aarch64-unknown-linux-gnu --release
# 若 pkg-config 报错：
# PKG_CONFIG_LIBDIR=/usr/lib/aarch64-linux-gnu/pkgconfig:/usr/share/pkgconfig \
# PKG_CONFIG_ALLOW_CROSS=1 cross build --target aarch64-unknown-linux-gnu --release
```

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

### 信令服务器部署（scp 方式，不用 docker-compose）

```bash
# 一键部署到公网服务器（编译 + scp + 远端启动 systemd 服务）
bash .github/skills/deploy-test/scripts/deploy-signal.sh
# 验证（REMOTE_HOST 来自 env.sh）
source .github/skills/deploy-test/scripts/env.sh
curl http://${REMOTE_HOST}:${SIGNAL_PORT}/health   # → {"status":"ok"}
```

---

## 关键依赖与版本约束

- **str0m `0.5.1`** — Pure Rust WebRTC，API 注意事项：
  - `Rtc::builder().build()` — 无 `add_ice_server()` 方法，ICE server 暂不通过 builder 配置
  - 本地候选用 `rtc.add_local_candidate(Candidate::host(addr, Protocol::Udp)?)`
  - 网络接收用 `Receive::new(Protocol::Udp, src, dst, &buf[..n])` 而非直接构造 struct
  - `Event` 无 `IceCandidate` 变体；本地候选需手动注册，远端候选用 `rtc.add_remote_candidate()`
  - 媒体写入用 `rtc.direct_api().stream_tx_by_mid(mid, None)` 获取 writer
  - `add_media()` 调用时第三个参数为 `stream_id: Option<String>`，**必须传 `Some("anykvm".into())`**，否则 SDP 无 `a=msid`，浏览器 `ontrack` 事件的 `streams[]` 为空，导致无法绑定 `<video>` srcObject
- **audiopus `0.3.0-rc.0`** — crates.io 上没有稳定的 `0.3`，必须用 rc 版本
- **serialport** — 通过 `ch9329` feature flag 设为可选依赖（避免 `libudev` 在 armv7 交叉编译报错）
- **cross** — 从 git 安装：`cargo install cross --git https://github.com/cross-rs/cross`
- **openh264 0.6.6** — SW H264 编码，YUYV→YUV420→Annex B 格式（start code `00 00 00 01`），1280×720 Constrained Baseline Level 3.1

---

## 项目约定

### Rust Agent

- 每个模块对应单一职责：`video.rs` / `audio.rs` / `hid.rs` / `webrtc.rs` / `signal_client.rs`
- 模块间通信全部走 `tokio::sync::mpsc`（媒体帧、HID 控制帧）或 `oneshot`（SDP offer）
- 错误处理用 `anyhow`，日志用 `tracing`（结构化，`RUST_LOG=any_kvm_agent=debug` 控制级别）
- HID 串口支持通过 `#[cfg(feature = "ch9329")]` 条件编译隔离，默认不编译
- Agent 连接信令时可传 `?name=<设备名>` 参数，Web 界面会显示该名称
- **服务安装路径**：`/usr/bin/any-kvm-agent`（主二进制），`/usr/bin/any-kvm-agent-wrapper`（启动包装，设置 DISPLAY=:99 并启动 Xvfb），`/etc/any-kvm-agent/config.toml`（配置）

### Config

- 设备端配置文件：`agent/config.toml`（参考 `config.toml.example`）
- 最少必填项：`signal.url`、`signal.room_id`
- `signal.url` 末尾**不含** `/ws` 路径；agent 内部自动拼接

### Web 前端

- 无构建步骤，纯静态文件，由 signal-server 通过 HTTP 静态伺服直接提供
- 服务器部署位置：`/root/Any-KVM/web/`（signal-server 以 `-web /root/Any-KVM/web` 参数启动）
- STUN 服务器内置（含国内友好节点），用户无需手动填写
- 服务器地址记忆在 localStorage，刷新页面后自动恢复
- `ontrack` 处理：`const stream = streams[0] || new MediaStream(); stream.addTrack(track); remoteVideo.srcObject = stream;`（当 agent 无 msid 时 streams[] 可能为空，需 fallback）

---

## 常见陷阱

1. **浏览器无视频画面（ontrack streams[] 为空）** — agent 的 `add_media()` 必须传 `stream_id: Some("anykvm".into())`，否则 SDP 缺少 `a=msid`；同时 web 端 ontrack 需用 `streams[0] || new MediaStream()` 防止 undefined
2. **video.rs 使用实际分辨率** — V4L2 `set_format` 返回的 `actual.width/height` 才是真实分辨率，不能用 config 中的请求值做 YUYV→YUV420 转换，否则图像花屏
3. **cross build 失败找不到 pkg-config** — 设置 `PKG_CONFIG_LIBDIR` 和 `PKG_CONFIG_ALLOW_CROSS=1`
4. **Docker 未运行** — cross/Dockerfile.armv7 依赖 Docker；编译前确认 `docker info` 无报错
5. **`~/.cargo/bin` 不在 PATH** — rustup 安装后需手动 `source ~/.zshrc` 或重开终端
6. **str0m API 破坏性变更** — str0m 0.4 → 0.5 有重大变更；`Receive`、`DatagramRecv`、`Event` 枚举均有改动，参考 `~/.cargo/registry/src/*/str0m-0.5.*/src/` 查阅实际 API
7. **`[features]` 位置** — Cargo.toml 中 `[features]` 必须放在 `[dependencies]` 块**之后**，否则解析错误
8. **`/api/agents` 无 CORS 问题** — 该接口已设置 `Access-Control-Allow-Origin: *`，Web 页面跨域调用正常
9. **H264 调试** — agent 启动后前 30 帧会保存到 `/tmp/debug_video.h264`，可用 `ffprobe -v error -show_streams /tmp/debug_video.h264` 验证流合法性

---

## 关键文件速览

| 文件                                         | 作用                                                                          |
| -------------------------------------------- | ----------------------------------------------------------------------------- |
| `agent/src/webrtc.rs`                        | str0m 引擎：ICE + DTLS + SRTP，SDP offer/answer，RTP 发送；stream_id="anykvm" |
| `agent/src/video.rs`                         | V4L2 采集 + openh264 编码；YUYV→YUV420 转换用 `actual` 分辨率                |
| `agent/src/signal_client.rs`                 | WebSocket 信令客户端，SDP/ICE 消息路由                                        |
| `agent/Dockerfile.armv7`                     | 玩客云 armv7l 交叉编译专用 Docker 镜像（Ubuntu 22.04 + armhf 工具链）         |
| `agent/Cross.toml`                           | cross 交叉编译：aarch64 目标，pre-build 安装系统库，pkg-config 环境透传       |
| `signal/main.go`                             | 信令服务器：WebSocket hub，SDP/ICE 转发，/health，/api/agents，静态文件       |
| `web/app.js`                                 | 浏览器端：Agent 发现列表、WebRTC 控制逻辑、HID DataChannel 消息发送           |
| `scripts/build-and-package.sh`               | 一键编译+打包：生成 .deb 和 .tar.gz，支持 --install 直接安装                  |
| `scripts/any-kvm-agent.service`              | systemd 服务单元（开机自启，打包时自动嵌入）                                  |
| `deploy/docker-compose.yml`                  | signal-server + coturn 容器编排，端口映射                                     |
| `.github/skills/deploy-test/scripts/env.sh` | 部署配置：REMOTE_HOST、REMOTE_USER/PASS、LOCAL_SUDO_PASS（不纳入版本控制）    |
| `docs/02-architecture.md`                    | 系统架构决策文档                                                              |

---

对话语言需要是中文
