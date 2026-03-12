# Any-KVM 架构设计文档

版本：1.0  
日期：2026-03-12

---

## 1. 总体架构

### 1.1 架构图

```
┌─────────────────────────────────────────────────────────────────────────┐
│                          控制端（Operator）                              │
│                                                                         │
│   浏览器（Chrome / Firefox / Edge）                                      │
│   ┌────────────────────────────────────────────────────────────────┐   │
│   │  Web 控制台（纯静态 HTML + CSS + JavaScript）                   │   │
│   │  ┌─────────────┐  ┌───────────────┐  ┌──────────────────────┐  │   │
│   │  │  视频渲染    │  │  音频播放      │  │  键鼠事件捕获         │  │   │
│   │  │  <video>    │  │  Web Audio    │  │  keydown/mousemove   │  │   │
│   │  └──────┬──────┘  └───────┬───────┘  └──────────┬───────────┘  │   │
│   │         │                 │                      │             │   │
│   │         └────────WebRTC RTCPeerConnection────────┘             │   │
│   │              Video Track / Audio Track / DataChannel           │   │
│   └────────────────────────────┬───────────────────────────────────┘   │
└────────────────────────────────│────────────────────────────────────────┘
                                 │  WebRTC P2P (UDP/DTLS/SRTP)
                                 │  ICE 打洞成功后直连（不经过服务器）
                                 │
              ┌──────────────────┴──────────────────┐
              │          可选 TURN 中继              │  ← 打洞失败才触发
              │    服务器带宽：≤ 520 kbps / 会话     │
              └──────────────────┬──────────────────┘
                                 │
┌────────────────────────────────│────────────────────────────────────────┐
│                  信令 / NAT 辅助服务器（弱 VPS）                         │
│                  固定公网 IP，带宽 < 1 Mbps                              │
│                                                                         │
│   ┌──────────────────────────────┐   ┌──────────────────────────────┐  │
│   │    signal-server（Go）       │   │    coturn                    │  │
│   │    WebSocket :8080/ws        │   │    STUN  :3478/UDP           │  │
│   │    静态文件 :8080/            │   │    TURN  :3478/UDP (限速)    │  │
│   │    信令带宽 < 5 KB/s          │   │    STUN 零带宽消耗           │  │
│   └──────────────────────────────┘   └──────────────────────────────┘  │
└────────────────────────────────│────────────────────────────────────────┘
                                 │  WebSocket 信令（SDP + ICE）
                                 │
┌────────────────────────────────│────────────────────────────────────────┐
│                  设备端（玩客云 KVM Device）                              │
│                  Amlogic S905L，1 GB RAM，Armbian                        │
│                                                                         │
│   ┌─────────────────────────────────────────────────────────────────┐  │
│   │              any-kvm-agent（Rust 单二进制）                       │  │
│   │                                                                  │  │
│   │  ┌──────────────┐  ┌──────────────┐  ┌─────────────────────┐   │  │
│   │  │  video 模块  │  │  audio 模块  │  │   hid 模块           │   │  │
│   │  │  V4L2 采集   │  │  ALSA 采集   │  │  USB Gadget / CH9329 │   │  │
│   │  │  H.264 编码  │  │  Opus 编码   │  │  HID 报文注入         │   │  │
│   │  └──────┬───────┘  └──────┬───────┘  └──────────┬──────────┘   │  │
│   │         │                 │                      │              │  │
│   │         └──────── webrtc 模块 (str0m) ───────────┘              │  │
│   │                   RTCPeerConnection                              │  │
│   │                   ICE Agent + DTLS + SRTP                       │  │
│   │                           │                                     │  │
│   │              signal_client 模块（WebSocket）                     │  │
│   └───────────────────────────┼─────────────────────────────────────┘  │
│                               │                                         │
│   HDMI USB 采集卡──V4L2──────►│   USB HID Gadget ─── /dev/hidg0/g1     │
│   HDMI 音频──ALSA────────────►│   CH9329 UART ──── /dev/ttyUSB0        │
└────────────────────────────────────────────────────────────────────────┘
```

### 1.2 核心原则

1. **服务器零媒体数据** — 服务器仅转发 JSON 信令消息（SDP/ICE），所有媒体数据走 WebRTC P2P，P2P 失败才触发 TURN 中继。  
2. **带宽预算刚性约束** — TURN 中继限速 480 kbps、并发 1 路；信令维持心跳带宽 < 5 KB/s。  
3. **单二进制 Agent** — 设备端所有功能打包为单一 Rust 二进制，`config.toml` 驱动，无外部依赖进程。  
4. **WebRTC 端到端加密** — SRTP（媒体）+ DTLS（DataChannel）+ WebSocket TLS（信令，生产环境）。

---

## 2. 模块设计

### 2.1 设备端 Agent（Rust）

```
agent/src/
├── main.rs            # 入口：读取配置，启动各模块，tokio runtime
├── config.rs          # 配置结构体，从 config.toml 反序列化
├── video.rs           # V4L2 采集 + H.264 编码（硬件优先/libx264 回退）
├── audio.rs           # ALSA PCM 采集 + Opus 编码
├── hid.rs             # USB HID Gadget 写 / CH9329 UART 写
├── webrtc.rs          # str0m RTCPeerConnection 封装，轨道管理
└── signal_client.rs   # WebSocket 客户端：连接信令服务器，SDP/ICE 交换
```

#### 2.1.1 video 模块

- 打开 V4L2 设备（默认 `/dev/video0`），枚举格式
- 优先请求 `V4L2_PIX_FMT_H264`（硬件编码，Amlogic H.264 M2M）
- 回退到 `V4L2_PIX_FMT_YUYV` + libx264 软编
- 编码参数：profile=baseline, level=3.1, 关键帧间隔 2s
- 输出 Annex-B NAL 帧，通过 tokio channel 送入 webrtc 模块

#### 2.1.2 audio 模块

- ALSA `hw:0,0` 或配置项指定设备
- 参数：48000 Hz、1 ch、S16_LE，帧大小 960 样本（20 ms）
- 使用 `audiopus` crate 编码为 Opus
- 输出 RTP-ready 帧，通过 channel 送入 webrtc 模块

#### 2.1.3 hid 模块

控制消息格式（DataChannel 二进制）：

```
HID 消息（8 字节）
┌────────┬────────┬────────────────────────────────────────────────┐
│ type   │ flags  │ payload (6 bytes)                               │
│ 1 byte │ 1 byte │                                                 │
└────────┴────────┴────────────────────────────────────────────────┘

type=0x01 键盘：flags=modifier, payload=[key1..key6] (6-KRO)
type=0x02 鼠标移动：payload=[abs_x:u16, abs_y:u16, 0, 0]
type=0x03 鼠标按键：flags=buttons, payload=[0..0]
type=0x04 鼠标滚轮：flags=delta_y (i8), payload=[0..0]
```

USB Gadget 路径：
- 键盘 → `/dev/hidg0`（Boot Keyboard descriptor）
- 鼠标 → `/dev/hidg1`（Absolute Mouse descriptor）

CH9329 回退：通过 UART 发送 CH9329 协议帧（9600 baud）

#### 2.1.4 webrtc 模块

- 使用 `str0m` crate（纯 Rust，无外部 C 依赖）
- 配置 ICE servers：多个公共 STUN + 自有 coturn STUN/TURN
- 创建 SDP offer（设备端为 offerer）
- 接收 SDP answer 后完成协商
- 处理 ICE candidate 交换（通过 signal_client）
- 注册 DataChannel 消息回调 → hid 模块

#### 2.1.5 signal_client 模块

- tokio-tungstenite WebSocket 客户端
- 连接 `ws(s)://server:8080/ws?room=<id>&role=device`
- 收到 `answer` → 送 webrtc 模块；收到 `candidate` → 送 webrtc ICE
- 断线指数退避重连（1s、2s、4s…最大 60s）

---

### 2.2 信令服务器（Go）

```
signal/
├── main.go    # 全部逻辑（< 300 行）
└── go.mod
```

- `GET /ws?room=<id>&role=<device|client>` — WebSocket 连接
- `GET /health` — JSON 状态接口
- `GET /` — 静态文件（Web 控制端）
- 内存 room map：每个 room 存 device + client 两个 peer
- 写循环单独 goroutine，避免并发写 websocket 错误
- 30s ping 心跳，120s 无响应断开

---

### 2.3 Web 控制端（静态）

```
web/
├── index.html   # 连接界面 + 控制台界面
├── style.css    # 暗色主题 UI
└── app.js       # WebRTC + 信令 + 键鼠捕获逻辑
```

**WebRTC 流程（浏览器端）**：
```
1. 用户填写 signalURL / roomID / ICE servers → 点击连接
2. 创建 RTCPeerConnection，配置 ICE servers
3. 打开 DataChannel "hid-control"
4. 连接信令 WebSocket（role=client）
5. 等待收到 {"type":"offer"} → setRemoteDescription(offer)
6. createAnswer() → setLocalDescription(answer)
7. 发送 {"type":"answer", payload: answer} 给信令服务器
8. 收到 {"type":"candidate"} → addIceCandidate()
9. 本地 ICE candidate → 发送给信令服务器
10. ICE 连接建立 → 接收 video/audio track → 显示
11. 捕获键鼠事件 → 编码为二进制帧 → DataChannel.send()
```

---

### 2.4 部署架构（服务器端）

```
deploy/
├── docker-compose.yml   # signal-server + coturn
├── Dockerfile.signal    # signal-server 镜像
└── coturn.conf          # STUN/TURN 配置
```

Docker Compose 端口：
- `8080/tcp` — 信令 WebSocket + 静态文件  
- `3478/udp` — STUN  
- `3478/tcp` — STUN/TURN over TCP  
- `49152-49200/udp` — TURN relay 端口范围（最小化开放，限 50 端口）

---

## 3. 数据流

### 3.1 连接建立流程

```
设备端 Agent                信令服务器              浏览器控制端
     │                          │                       │
     ├──WS connect(role=device)─►│                       │
     │                          │◄──WS connect(role=client)
     │                          │                       │
     ├──createOffer()            │                       │
     ├──{type:offer}────────────►│──{type:offer}─────────►│
     │                          │                       ├──setRemoteDesc
     │                          │                       ├──createAnswer
     │◄──{type:answer}────────────◄──{type:answer}────────┤
     ├──setRemoteDesc             │                       │
     │                          │                       │
     ├──{type:candidate}─────────►│──{type:candidate}─────►│
     │◄──{type:candidate}─────────◄──{type:candidate}──────┤
     │                          │                       │
     ├──────────── ICE 打洞，建立 P2P UDP 通道 ────────────┤
     │                          │                       │
     ├══════════ SRTP Video Track (H.264) ══════════════►│
     ├══════════ SRTP Audio Track (Opus)  ══════════════►│
     ├◄═════════ DTLS DataChannel (HID)   ═══════════════┤
```

### 3.2 视频数据流

```
HDMI 信号
  → USB 采集卡（UVC）
  → V4L2 设备 /dev/video0
  → [尝试] V4L2 M2M H.264 硬件编码
  → [失败] YUYV → libx264 Baseline H.264
  → Annex-B NAL 单元
  → str0m RTP 打包（H.264 RTP payload format RFC 6184）
  → SRTP 加密
  → UDP → ICE P2P 通道
  → 浏览器 RTCPeerConnection 解包
  → 浏览器内置 H.264 解码
  → <video> 元素渲染
```

### 3.3 HID 控制数据流

```
浏览器键盘/鼠标事件
  → JavaScript 事件回调
  → 编码为 8 字节二进制帧
  → RTCDataChannel.send(ArrayBuffer)
  → DTLS 加密
  → UDP → ICE P2P 通道
  → str0m DataChannel 接收
  → hid 模块解码
  → USB HID Gadget write(/dev/hidg0 or /dev/hidg1)
  → 被控设备 USB HID 输入
```

---

## 4. 关键技术决策

| 决策点 | 选择 | 备选 | 原因 |
|--------|------|------|------|
| Agent 语言 | Rust | C, Go | 内存安全、单二进制、ARM 交叉编译成熟 |
| WebRTC 库 | str0m | webrtc-rs | 纯 Rust 无 C 依赖，内存占用更小 |
| 视频编码 | H.264 Baseline | VP8, H.265 | 浏览器兼容性最佳，硬件支持广泛 |
| 音频编码 | Opus 16kbps | AAC | WebRTC 标准，低带宽音质优 |
| 信令语言 | Go | Node.js, Python | 单二进制，内存 < 10 MB，并发好 |
| HID 方案 | USB Gadget 优先 | CH9329 | 零外部硬件，延迟最低 |
| NAT 穿透 | ICE + STUN + TURN | 端口转发/Tailscale | P2P 优先，服务器零媒体带宽 |
| 部署方式 | Docker Compose | 裸机 | 服务器一键部署，隔离运行环境 |

---

## 5. 安全设计

| 层级 | 机制 |
|------|------|
| 媒体传输 | SRTP（WebRTC 强制，AES-128-CM） |
| 控制通道 | DTLS 1.2/1.3（DataChannel） |
| 信令传输 | WebSocket（开发）/ WSS/TLS（生产） |
| TURN 认证 | coturn HMAC-SHA1 时效凭证（用户名+密码） |
| 会话隔离 | room ID 隔离，服务器不解析媒体内容 |
| 访问控制 | 生产建议：在信令服务器增加 room ID 鉴权 token |

---

## 6. 性能估算

### 玩客云 CPU 预算（Amlogic S905L，4×Cortex-A53 @1.5GHz）

| 任务 | 预估 CPU |
|------|---------|
| V4L2 采集 720p | ~5% |
| libx264 软编 720p@15fps | ~60–70% |
| V4L2 M2M 硬编 720p@15fps | ~10–15% |
| ALSA + Opus 编码 | ~3% |
| str0m WebRTC（SRTP/ICE） | ~5–8% |
| 系统开销 | ~5% |
| **合计（软编）** | **~80%** |
| **合计（硬编）** | **~28%** |

> **结论**：优先验证 Amlogic V4L2 M2M 硬编可用性；若不可用，软编 720p@15fps 接近 CPU 上限，可降为 480p@15fps（软编约 40%）。

---

## 7. 目录结构

```
Any-KVM/
├── docs/
│   ├── 01-requirements.md      # 需求规格
│   └── 02-architecture.md      # 本文档
├── agent/                      # Rust 设备端
│   ├── Cargo.toml
│   ├── Cross.toml              # 交叉编译配置
│   ├── config.toml.example     # 示例配置
│   └── src/
│       ├── main.rs
│       ├── config.rs
│       ├── video.rs
│       ├── audio.rs
│       ├── hid.rs
│       ├── webrtc.rs
│       └── signal_client.rs
├── signal/                     # Go 信令服务器
│   ├── main.go
│   └── go.mod
├── web/                        # 浏览器控制端
│   ├── index.html
│   ├── style.css
│   └── app.js
├── deploy/                     # 服务器部署
│   ├── docker-compose.yml
│   ├── Dockerfile.signal
│   └── coturn.conf
├── scripts/                    # 设备端辅助脚本
│   └── setup-hid-gadget.sh     # USB HID Gadget 配置脚本
└── README.md
```
