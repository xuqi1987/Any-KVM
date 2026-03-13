#!/usr/bin/env bash
# run-agent.sh — 在本机生成 config.toml 并运行 agent（前台，Ctrl-C 停止）
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/env.sh"

CONFIG_FILE="${AGENT_DIR}/config.toml"

echo "==> [1/3] 检查 agent 二进制"
if [ ! -f "${AGENT_BIN}" ]; then
    echo "  未找到 release 二进制，先编译..."
    bash "${SCRIPT_DIR}/build-agent.sh"
fi

echo "==> [2/3] 生成 config.toml → ${CONFIG_FILE}"
# 检测显示器
DISPLAY_VAL="${DISPLAY:-}"
if [ -z "${DISPLAY_VAL}" ]; then
    # 尝试自动探测 X11 display
    if [ -S /tmp/.X11-unix/X0 ]; then
        DISPLAY_VAL=":0"
    fi
fi

# 检测是否有 v4l2 设备
VIDEO_SOURCE="screen"
if ls /dev/video* >/dev/null 2>&1; then
    VIDEO_DEVICE=$(ls /dev/video* | head -1)
    echo "  检测到视频设备 ${VIDEO_DEVICE}，使用 v4l2 源（失败会自动回退屏幕截图）"
    VIDEO_SOURCE="v4l2"
else
    echo "  未检测到 /dev/video*，使用屏幕截图源"
fi

cat > "${CONFIG_FILE}" <<EOF
[signal]
url     = "${SIGNAL_URL}"
room_id = "${ROOM_ID}"

[video]
source       = "${VIDEO_SOURCE}"
device       = "${VIDEO_DEVICE:-/dev/video0}"
width        = 1280
height       = 720
fps          = 15
bitrate_kbps = 500
hw_encode    = false

[audio]
device       = "default"
sample_rate  = 48000
channels     = 1
bitrate_kbps = 16
enabled      = false

[hid]
mode            = "gadget"
keyboard_device = "/dev/hidg0"
mouse_device    = "/dev/hidg1"
serial_port     = "/dev/ttyUSB0"
serial_baud     = 9600

[ice]
stun_servers = [
    "stun:stun.l.google.com:19302",
    "stun:stun1.l.google.com:19302",
    "stun:stun.cloudflare.com:3478",
]
# turn_url      = "turn:${REMOTE_HOST}:3478"
# turn_username = "kvmuser"
# turn_password = "change_me_in_production"
EOF

echo "  config.toml 已写入"
cat "${CONFIG_FILE}"

echo ""
echo "==> [3/3] 启动 agent（Ctrl-C 停止）"
echo "   信令: ${SIGNAL_URL}"
echo "   房间: ${ROOM_ID}"
echo "   视频: ${VIDEO_SOURCE}"
[ -n "${DISPLAY_VAL}" ] && export DISPLAY="${DISPLAY_VAL}"
RUST_LOG=any_kvm_agent=debug exec "${AGENT_BIN}" "${CONFIG_FILE}"
