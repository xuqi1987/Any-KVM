# Any-KVM 需求规格说明书

版本：1.0  
日期：2026-03-12  
状态：已确认

---

## 1. 项目背景

本系统实现一套轻量级远程 KVM（Keyboard Video Mouse）控制平台。  
物理设备端运行在资源受限的 ARM Linux 盒子（首要目标：玩客云，Amlogic S905L，1 GB RAM）上，控制端通过普通浏览器访问，双端通过 WebRTC P2P 直连传输媒体数据。  
云服务器仅承担信令中继和 NAT 辅助穿透，**带宽需求 < 5 KB/s**，可运行在 1 核 512 MB、极低带宽（< 1 Mbps）的廉价 VPS 上。

---

## 2. 用户角色

| 角色 | 描述 |
|------|------|
| **操作者（Operator）** | 通过浏览器远程控制被控机器的人 |
| **设备管理员** | 在玩客云上部署和维护 Agent 的人 |
| **服务器管理员** | 在云服务器上部署信令服务器和 coturn 的人 |

---

## 3. 功能需求

### 3.1 视频采集与传输（FR-VID）

| ID | 需求 |
|----|------|
| FR-VID-01 | 系统支持通过 V4L2 接口读取 USB HDMI 采集卡的视频信号 |
| FR-VID-02 | 系统优先使用 V4L2 M2M 硬件 H.264 编码（Amlogic 硬件编码器） |
| FR-VID-03 | 硬件编码不可用时，回退到 libx264 软件编码 |
| FR-VID-04 | 推荐视频参数：1280×720 @ 15fps，目标码率 500 kbps |
| FR-VID-05 | 低带宽模式：640×360 @ 10fps，目标码率 300 kbps |
| FR-VID-06 | 编码器使用 H.264 Baseline/Constrained Baseline Profile（WebRTC 兼容） |
| FR-VID-07 | 视频通过 WebRTC 视频轨以 RTP/SRTP 方式传输 |
| FR-VID-08 | 控制端通过浏览器原生 H.264 解码器渲染视频，不需要插件 |

### 3.2 音频采集与传输（FR-AUD）

| ID | 需求 |
|----|------|
| FR-AUD-01 | 系统通过 ALSA 接口采集音频（HDMI 采集卡音频通道） |
| FR-AUD-02 | 采集参数：48000 Hz、单声道、16-bit 有符号整数 |
| FR-AUD-03 | 使用 Opus 编码，目标码率 16 kbps |
| FR-AUD-04 | 音频通过 WebRTC 音频轨以 RTP/SRTP 方式传输 |
| FR-AUD-05 | 控制端浏览器原生解码 Opus 并播放 |
| FR-AUD-06 | 音频为可选功能，不影响核心视频和控制功能 |

### 3.3 键鼠控制（FR-HID）

| ID | 需求 |
|----|------|
| FR-HID-01 | 浏览器捕获键盘（keydown/keyup）、鼠标（move/click/wheel）事件 |
| FR-HID-02 | 控制数据通过 WebRTC DataChannel 传输 |
| FR-HID-03 | 设备端优先通过 USB HID Gadget（`/dev/hidg0` 键盘、`/dev/hidg1` 鼠标）注入输入 |
| FR-HID-04 | USB OTG 不可用时，回退到外部 CH9329 HID 芯片（UART 串口驱动） |
| FR-HID-05 | 鼠标支持绝对坐标模式（坐标范围 0–32767） |
| FR-HID-06 | 键盘支持所有标准按键和 Ctrl/Alt/Shift/Meta 修饰键 |
| FR-HID-07 | 支持发送 Ctrl+Alt+Del 组合键 |
| FR-HID-08 | 控制数据协议使用紧凑二进制格式，每条消息 ≤ 16 字节 |

### 3.4 网络与 NAT 穿透（FR-NET）

| ID | 需求 |
|----|------|
| FR-NET-01 | 系统使用 WebRTC ICE 机制实现 NAT 穿透 |
| FR-NET-02 | 配置多个公共 STUN 服务器以提高打洞成功率 |
| FR-NET-03 | 部署 coturn 提供自有 STUN 服务（零带宽消耗） |
| FR-NET-04 | 部署 coturn 提供 TURN 中继兜底，限速 480 kbps、并发 1 路 |
| FR-NET-05 | P2P 建立成功时服务器带宽 < 5 KB/s（仅信令维持心跳） |
| FR-NET-06 | TURN 中继时服务器带宽 ≤ 520 kbps（单路视频 + 音频） |

### 3.5 信令服务器（FR-SIG）

| ID | 需求 |
|----|------|
| FR-SIG-01 | 信令服务器通过 WebSocket 提供服务 |
| FR-SIG-02 | 支持 room 模式：每个房间 ID 对应一个设备端 + 一个客户端 |
| FR-SIG-03 | 转发 SDP offer、SDP answer 和 ICE candidate 消息 |
| FR-SIG-04 | 服务器无需解析媒体内容，透明转发 JSON 消息 |
| FR-SIG-05 | 静态文件服务：同时提供 Web 控制端静态资源 |

### 3.6 Web 控制端（FR-WEB）

| ID | 需求 |
|----|------|
| FR-WEB-01 | 纯浏览器实现，无需安装任何插件或客户端 |
| FR-WEB-02 | 连接界面：配置信令服务器地址、房间 ID、ICE 服务器 |
| FR-WEB-03 | 控制台界面：实时视频显示、状态指示 |
| FR-WEB-04 | 支持全屏模式 |
| FR-WEB-05 | 显示连接质量（P2P 直连 / TURN 中继 / 断线）状态 |
| FR-WEB-06 | 音频开关控制 |
| FR-WEB-07 | 发送 Ctrl+Alt+Del 快捷键按钮 |

---

## 4. 非功能需求

### 4.1 性能（NFR-PERF）

| ID | 需求 |
|----|------|
| NFR-PERF-01 | 端到端延迟（网络条件良好，P2P 连接）：≤ 150 ms |
| NFR-PERF-02 | Agent 进程内存占用：≤ 128 MB |
| NFR-PERF-03 | 信令服务器内存占用：≤ 32 MB |
| NFR-PERF-04 | 玩客云 CPU 占用（软件编码 720p15）：≤ 80%；硬件编码：≤ 30% |
| NFR-PERF-05 | 视频启动时间（从建立 WebRTC 连接到首帧显示）：≤ 3 s |

### 4.2 可靠性（NFR-REL）

| ID | 需求 |
|----|------|
| NFR-REL-01 | 信令服务器 30 秒心跳，断线自动检测 |
| NFR-REL-02 | ICE 连接断开后，WebRTC 自动重连（前端触发） |
| NFR-REL-03 | Agent 进程崩溃时 systemd 自动重启，30 s 内恢复 |

### 4.3 安全性（NFR-SEC）

| ID | 需求 |
|----|------|
| NFR-SEC-01 | WebRTC 媒体流通过 SRTP 端对端加密 |
| NFR-SEC-02 | DataChannel 通过 DTLS 加密 |
| NFR-SEC-03 | 信令服务器通过 room ID 隔离会话（不保存任何媒体数据） |
| NFR-SEC-04 | coturn 配置 TURN 认证（用户名/密码），防止 TURN 滥用 |
| NFR-SEC-05 | 生产部署建议对信令服务器启用 TLS（WSS） |

### 4.4 可维护性（NFR-MAINT）

| ID | 需求 |
|----|------|
| NFR-MAINT-01 | Agent 以单一二进制文件分发，配置通过 `config.toml` 文件管理 |
| NFR-MAINT-02 | 信令服务器通过 Docker Compose 部署 |
| NFR-MAINT-03 | Web 控制端为纯静态文件，可由信令服务器直接托管 |

---

## 5. 硬件约束

| 组件 | 要求 |
|------|------|
| KVM 设备 | Linux ARM/x86，支持 V4L2，USB OTG（可选） |
| 首要目标硬件 | 玩客云（Amlogic S905L，ARMv8，1 GB RAM，Armbian） |
| HDMI 采集卡 | 标准 USB UVC 采集卡，支持 MJPEG 或 H.264 输出 |
| 信令服务器 | 1 核 512 MB，固定公网 IP，带宽 < 1 Mbps |
| 控制端 | 任意现代浏览器（Chrome 80+、Firefox 75+、Edge 80+） |

---

## 6. 带宽预算

| 连接模式 | 服务器带宽 | 说明 |
|----------|-----------|------|
| P2P 直连 | < 5 KB/s | 仅 WebSocket 心跳信令 |
| TURN 中继（标准） | ≈ 520 kbps | 视频 500k + 音频 16k + DataChannel 5k |
| TURN 中继（低带宽模式） | ≈ 320 kbps | 视频 300k + 音频 16k |

---

## 7. 开发优先级

| 优先级 | 功能 |
|--------|------|
| P0（必须） | 视频采集 + WebRTC 传输 + 浏览器显示 |
| P0（必须） | 键盘和鼠标控制（HID Gadget） |
| P1（重要） | NAT 穿透（STUN + TURN） |
| P1（重要） | 音频传输 |
| P2（可选） | CH9329 HID 回退 |
| P2（可选） | TURN 带宽限速配置 |
| P3（未来） | 多设备管理、用户认证、带宽自适应、录像 |
