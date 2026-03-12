# Any-KVM

轻量级远程 KVM 控制平台。通过 **WebRTC P2P 直连**在浏览器中实时访问并控制物理设备，服务器仅承担 **信令交换和 NAT 穿透辅助**，不转发任何视频/音频数据。

---

## 功能

| 功能 | 说明 |
|------|------|
| 视频采集 | HDMI USB 采集卡（V4L2），H.264 硬件编码优先 / libx264 软编回退 |
| 音频传输 | ALSA 采集 + Opus 16 kbps 编码 |
| 键鼠控制 | USB HID Gadget（/dev/hidg0/g1）/ CH9329 UART 芯片，WebRTC DataChannel 传输 |
| NAT 穿透 | ICE + 多 STUN 服务器 + coturn TURN 兜底（P2P 优先） |
| 控制端 | 纯浏览器 Web UI（无需插件），Chrome/Firefox/Edge |
| 服务器 | 信令带宽 < 5 KB/s；TURN 中继限速 480 kbps / 1 路并发 |

---

## 快速开始

### 1. 服务器部署（1 核 512 MB VPS）

```bash
git clone https://github.com/YOUR_NAME/Any-KVM.git && cd Any-KVM

# 编辑 coturn 配置，填入公网 IP 和 TURN 密码
vim deploy/coturn.conf   # 替换 YOUR_PUBLIC_IP 和 CHANGE_ME_IN_PRODUCTION

cd deploy
docker compose up -d
curl http://localhost:8080/health
```

服务器开放端口：`8080/tcp`（信令+Web）、`3478/udp`（STUN/TURN）、`49152-49200/udp`（TURN relay）

---

### 2. 玩客云设备端部署

#### 2.1 刷入 Armbian

参考 [One-KVM 玩客云文档](https://docs.one-kvm.cn/python/start_install/onecloud_install/) 刷入 Armbian，确认 `/dev/video0` 可识别。

#### 2.2 配置 USB HID Gadget

```bash
scp scripts/setup-hid-gadget.sh root@<onecloud-ip>:/tmp/
ssh root@<onecloud-ip> "bash /tmp/setup-hid-gadget.sh"
ls /dev/hidg*   # 应出现 hidg0（键盘）、hidg1（鼠标）
```

开机自启（加入 `/etc/rc.local`）：
```bash
echo "bash /opt/any-kvm/setup-hid-gadget.sh" >> /etc/rc.local
```

#### 2.3 编译 Agent（开发机上交叉编译）

```bash
cargo install cross
cd agent
cross build --target aarch64-unknown-linux-gnu --release
scp target/aarch64-unknown-linux-gnu/release/any-kvm-agent root@<onecloud-ip>:/opt/any-kvm/
scp config.toml.example root@<onecloud-ip>:/opt/any-kvm/config.toml
```

#### 2.4 配置并启动 Agent

```bash
ssh root@<onecloud-ip>
vim /opt/any-kvm/config.toml
```

最少需要修改的配置项：

```toml
[signal]
url     = "ws://your-server:8080/ws"
room_id = "kvm-room-1"

[video]
hw_encode = true   # 尝试 Amlogic 硬件编码

[ice]
turn_url      = "turn:your-server:3478"
turn_username = "kvmuser"
turn_password = "CHANGE_ME"
```

创建 systemd 服务：

```bash
cat > /etc/systemd/system/any-kvm.service << 'EOF'
[Unit]
Description=Any-KVM Agent
After=network.target

[Service]
ExecStart=/opt/any-kvm/any-kvm-agent /opt/any-kvm/config.toml
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF

systemctl enable --now any-kvm
```

---

### 3. 打开控制台

浏览器访问 `http://your-server:8080`，填入信令地址和房间 ID 后点击**连接**。

- **点击视频区域**激活键鼠控制，**Esc** 释放
- **⌨ CAD** 发送 Ctrl+Alt+Del
- 状态栏 `✅ P2P 直连` 表示流量不经过服务器

---

## 验证与检查

### 1. 服务器是否正常

```bash
# 检查容器运行状态
docker compose ps

# 信令服务器健康检查
curl http://localhost:8080/health
# 正常返回: {"status":"ok"}

# 查看信令服务器日志
docker compose logs -f signal

# 检查 coturn 是否监听
ss -ulnp | grep 3478

# 从外网测试 STUN（本机执行）
# macOS: brew install stunman  /  Linux: apt install stun-client
stun your-server-ip 3478
# 正常输出: "Nat Type: ..." 以及分配到的外部 IP
```

### 2. 玩客云视频采集是否正常

```bash
# 确认采集卡被识别
v4l2-ctl --list-devices
# 正常输出例: USB Video: USB Video (video0)

# 查看支持的格式
v4l2-ctl -d /dev/video0 --list-formats-ext
# 应包含 H264 或 MJPG

# 抓一帧图片验证画面
apt install -y ffmpeg
ffmpeg -f v4l2 -i /dev/video0 -frames:v 1 /tmp/test.jpg
scp root@<onecloud-ip>:/tmp/test.jpg ~/Desktop/
# 在本机打开 test.jpg，确认有画面

# 验证 H.264 硬件编码输出
ffmpeg -f v4l2 -input_format h264 -i /dev/video0 -c copy -t 5 /tmp/test.ts 2>&1 | tail -5
```

### 3. USB HID 键鼠是否正常

```bash
# 确认 HID 设备节点存在
ls -la /dev/hidg*
# 应有 hidg0（键盘）和 hidg1（鼠标）

# 往键盘设备写一个空报告（测试可写性，不会触发按键）
printf '\x00\x00\x00\x00\x00\x00\x00\x00' > /dev/hidg0
echo $?   # 返回 0 表示成功

# 测试鼠标移动（向右下移动 10px）
printf '\x00\x0a\x0a\x00\x00\x00' > /dev/hidg1
echo $?   # 返回 0 表示成功

# CH9329 串口方式：检查串口设备
ls /dev/ttyUSB* /dev/ttyS*
stty -F /dev/ttyUSB0 speed   # 应为 9600
```

### 4. 端到端联调

启动 Agent 后观察日志关键字：

```bash
journalctl -u any-kvm -f
```

| 日志关键字 | 含义 |
|------------|------|
| `connected to signal server` | 信令连接成功 |
| `ICE connected` | WebRTC P2P 建立 |
| `video track started` | 视频流开始推送 |
| `audio track started` | 音频流开始推送 |
| `HID channel open` | 控制通道就绪 |

浏览器按 F12 打开控制台，`iceConnectionState: connected` 表示 P2P 直连成功。

---

## 目录结构

```
Any-KVM/
├── docs/
│   ├── 01-requirements.md    # 需求规格说明书
│   └── 02-architecture.md    # 系统架构设计文档
├── agent/                    # Rust 设备端 Agent
│   ├── Cargo.toml
│   ├── Cross.toml
│   ├── config.toml.example
│   └── src/
│       ├── main.rs           # 入口
│       ├── config.rs         # 配置读取
│       ├── video.rs          # V4L2 + H.264 编码
│       ├── audio.rs          # ALSA + Opus 编码
│       ├── hid.rs            # USB HID Gadget / CH9329
│       ├── webrtc.rs         # str0m WebRTC 引擎
│       └── signal_client.rs  # 信令 WebSocket 客户端
├── signal/                   # Go 信令服务器
├── web/                      # 浏览器控制端（纯静态）
├── deploy/                   # 服务器部署（Docker Compose + coturn）
└── scripts/
    └── setup-hid-gadget.sh   # USB HID Gadget 配置脚本
```

---

## 带宽说明

| 场景 | 服务器带宽 |
|------|-----------|
| P2P 直连（大多数家庭/公司网络） | < 1 KB/s |
| TURN 中继（对称 NAT 等极端情况） | ≤ 520 kbps |
| 低带宽模式（480p@10fps 300kbps） | ≤ 320 kbps |

---

## 端到端延迟参考

| 环节 | 典型值 |
|------|--------|
| V4L2 采集 + H.264 硬编 | ~20 ms |
| H.264 软编（720p） | ~30 ms |
| WebRTC P2P 网络传输 | 20–80 ms |
| 浏览器解码渲染 | ~10 ms |
| **端到端合计** | **~50–120 ms** |

---

## 问题排查

**视频无法显示** → 检查 `v4l2-ctl --list-devices`，尝试降低分辨率至 480p@10fps

**ICE 一直连接中** → 确认 `3478/udp` 开放，检查 coturn 日志中 TURN 认证错误

**HID 无响应** → 重新运行 `setup-hid-gadget.sh`，确认 `/dev/hidg0` 存在

**CPU 占用过高** → 启用硬件编码，或将配置降为 `width=640, height=480, bitrate_kbps=300`

---

## 参考项目

- [One-KVM](https://github.com/mofeng-git/One-KVM) — 玩客云 KVM 移植，本项目的重要参考
- [str0m](https://github.com/algesten/str0m) — 纯 Rust WebRTC 库

---

## License

MIT