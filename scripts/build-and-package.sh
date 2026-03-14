#!/bin/bash
# =============================================================================
# Any-KVM Agent — 一键编译 & 打包脚本
#
# 用法：
#   bash scripts/build-and-package.sh [选项]
#
# 选项：
#   --server <IP或域名>   信令服务器地址（如 your-server.com 或公网 IP），必填
#   --room   <房间ID>     设备房间 ID（默认：主机名）
#   --port   <端口>       信令服务器端口（默认：8080）
#   --install             打包完成后直接安装到本机并启动服务
#   --no-deb              跳过 .deb 打包，只生成 tar.gz
#   --help                显示帮助
#
# 示例：
#   bash scripts/build-and-package.sh --server <YOUR_SERVER_IP> --install
#   bash scripts/build-and-package.sh --server my.server.com --room pi-living-room
#
# 提示：服务器 IP 统一在 deploy/env.example（复制为 env.sh 后填入真实值）中管理
# =============================================================================
set -eo pipefail

# ─── 颜色输出 ─────────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'
BLUE='\033[0;34m'; BOLD='\033[1m'; NC='\033[0m'
info()  { echo -e "${BLUE}[INFO]${NC} $*"; }
ok()    { echo -e "${GREEN}[OK]${NC}   $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC} $*"; }
die()   { echo -e "${RED}[ERR]${NC}  $*" >&2; exit 1; }

# ─── 解析参数 ─────────────────────────────────────────────────────────────────
OPT_INSTALL=false
OPT_NO_DEB=false
OPT_SERVER=""
OPT_ROOM=""
OPT_PORT="8080"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --server)  OPT_SERVER="$2"; shift 2 ;;
        --room)    OPT_ROOM="$2";   shift 2 ;;
        --port)    OPT_PORT="$2";   shift 2 ;;
        --install) OPT_INSTALL=true; shift ;;
        --no-deb)  OPT_NO_DEB=true;  shift ;;
        --help)
            sed -n '2,19p' "$0" | sed 's/^# \?//'
            exit 0 ;;
        *) die "未知参数: $1（使用 --help 查看帮助）" ;;
    esac
done

# ─── 路径定位 ─────────────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
AGENT_DIR="$PROJECT_ROOT/agent"
DIST_DIR="$PROJECT_ROOT/dist"

# ─── 写入 systemd 服务文件的函数 ──────────────────────────────────────────────
write_service_file() {
    local dest="$1"
    cat > "$dest" << 'SVC_EOF'
[Unit]
Description=Any-KVM Device Agent
Documentation=https://github.com/any-kvm/any-kvm
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=root
Environment=DISPLAY=:99
ExecStartPre=-/usr/bin/pkill -f "Xvfb :99"
ExecStart=/usr/bin/any-kvm-agent-wrapper
ExecStopPost=-/usr/bin/pkill -f "Xvfb :99"
Restart=on-failure
RestartSec=5s
StandardOutput=journal
StandardError=journal
SyslogIdentifier=any-kvm-agent
SupplementaryGroups=video audio input

[Install]
WantedBy=multi-user.target
SVC_EOF
}
generate_service_file() { write_service_file "$1"; }

# ─── 版本 & 架构 ──────────────────────────────────────────────────────────────
PKG_VERSION="0.1.0"
RAW_ARCH="$(uname -m)"
case "$RAW_ARCH" in
    x86_64)        DEB_ARCH="amd64"; CARGO_TARGET="x86_64-unknown-linux-gnu" ;;
    aarch64|arm64) DEB_ARCH="arm64"; CARGO_TARGET="aarch64-unknown-linux-gnu" ;;
    armv7l)        DEB_ARCH="armhf"; CARGO_TARGET="armv7-unknown-linux-gnueabihf" ;;
    *)             DEB_ARCH="$RAW_ARCH"; CARGO_TARGET="" ;;
esac
PKG_NAME="any-kvm-agent_${PKG_VERSION}_${DEB_ARCH}"

echo -e "${BOLD}╔══════════════════════════════════════╗${NC}"
echo -e "${BOLD}║    Any-KVM Agent 一键编译 & 打包     ║${NC}"
echo -e "${BOLD}╚══════════════════════════════════════╝${NC}"
echo ""
info "架构:     $RAW_ARCH  →  deb: $DEB_ARCH"
info "版本:     $PKG_VERSION"
info "输出目录: $DIST_DIR"
echo ""

# ─── 交互式询问服务器信息 ─────────────────────────────────────────────────────
# 若未通过 --server 传入且是交互终端，则询问用户
if [[ -z "$OPT_SERVER" ]]; then
    if [[ -t 0 ]]; then
        echo -e "${YELLOW}需要填写信令服务器信息（用于生成设备配置）${NC}"
        echo ""
        read -rp "  信令服务器 IP 或域名（参考 deploy/env.example 中的 REMOTE_HOST）: " OPT_SERVER
        read -rp "  服务器端口 [默认: 8080]: " _port
        [[ -n "$_port" ]] && OPT_PORT="$_port"
        read -rp "  设备房间 ID [默认: $(hostname -s)]: " _room
        [[ -n "$_room" ]] && OPT_ROOM="$_room"
        echo ""
    else
        die "请通过 --server <IP> 指定信令服务器地址（非交互模式）"
    fi
fi

[[ -z "$OPT_SERVER" ]] && die "服务器地址不能为空，请使用 --server <IP>"
[[ -z "$OPT_ROOM"   ]] && OPT_ROOM="$(hostname -s 2>/dev/null || echo 'kvm-device')"

SIGNAL_URL="ws://${OPT_SERVER}:${OPT_PORT}/ws"

ok "信令服务器: ${SIGNAL_URL}"
ok "设备房间ID: ${OPT_ROOM}"
echo ""

# ─── 生成 config.toml ─────────────────────────────────────────────────────────
# 将会嵌入安装包，安装时直接写入 /etc/any-kvm-agent/config.toml
_generate_config() {
    local dest="$1"
    # 自动检测视频源：有 V4L2 设备用 v4l2，否则用屏幕截图
    local video_source="screen"
    if [ -e /dev/video0 ]; then
        video_source="v4l2"
    fi
    cat > "$dest" << EOF
# Any-KVM Agent 配置文件
# 由 build-and-package.sh 自动生成于 $(date '+%Y-%m-%d %H:%M:%S')

[signal]
url     = "${SIGNAL_URL}"
room_id = "${OPT_ROOM}"

[video]
source       = "${video_source}"
device       = "/dev/video0"
width        = 1280
height       = 720
fps          = 15
bitrate_kbps = 500
hw_encode    = true

[audio]
device       = "default"
sample_rate  = 48000
channels     = 1
bitrate_kbps = 16
enabled      = true

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
    "stun:stun.miwifi.com:3478",
]
turn_url      = "turn:${OPT_SERVER}:3478"
turn_username = "kvmuser"
turn_password = "anykvm2026"
EOF
}

# ─── 1. 检查 / 安装系统依赖 ───────────────────────────────────────────────────
info "检查系统依赖…"

MISSING_PKGS=()
for pkg in pkg-config libasound2-dev libssl-dev libv4l-dev libclang-dev build-essential; do
    if ! dpkg -s "$pkg" &>/dev/null 2>&1; then
        case "$pkg" in
            pkg-config)      command -v pkg-config &>/dev/null || MISSING_PKGS+=("$pkg") ;;
            libasound2-dev)  pkg-config --exists alsa 2>/dev/null || MISSING_PKGS+=("$pkg") ;;
            libssl-dev)      pkg-config --exists openssl 2>/dev/null || MISSING_PKGS+=("$pkg") ;;
            libv4l-dev)      [ -f /usr/include/linux/videodev2.h ] || MISSING_PKGS+=("$pkg") ;;
            libclang-dev)    ldconfig -p 2>/dev/null | grep -q libclang || MISSING_PKGS+=("$pkg") ;;
            build-essential) command -v gcc &>/dev/null || MISSING_PKGS+=("gcc / build-essential") ;;
        esac
    fi
done

if [ ${#MISSING_PKGS[@]} -gt 0 ]; then
    warn "缺少以下系统库：${MISSING_PKGS[*]}"
    if command -v apt-get &>/dev/null; then
        info "检测到 Debian/Ubuntu，尝试自动安装…"
        sudo apt-get update -qq
        sudo apt-get install -y --no-install-recommends \
            pkg-config libasound2-dev libssl-dev libv4l-dev libclang-dev build-essential dpkg-dev
        ok "系统依赖安装完成"
    elif command -v dnf &>/dev/null; then
        sudo dnf install -y pkgconf alsa-lib-devel openssl-devel v4l-utils-devel clang-devel gcc
        ok "系统依赖安装完成"
    elif command -v pacman &>/dev/null; then
        sudo pacman -S --noconfirm pkgconf alsa-lib openssl v4l-utils clang base-devel
        ok "系统依赖安装完成"
    else
        die "请手动安装：pkg-config libasound2-dev(或同等包) libssl-dev libv4l-dev libclang-dev gcc"
    fi
else
    ok "系统依赖已就绪"
fi

# ─── 2. 检查 / 安装 Rust 工具链 ──────────────────────────────────────────────
info "检查 Rust 工具链…"
if ! command -v cargo &>/dev/null; then
    warn "未检测到 Rust，正在通过 rustup 安装…"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
    # shellcheck source=/dev/null
    source "$HOME/.cargo/env"
    ok "Rust 安装完成"
else
    ok "Rust $(rustc --version)"
fi
command -v cargo &>/dev/null || export PATH="$HOME/.cargo/bin:$PATH"

# ─── 3. 编译 ──────────────────────────────────────────────────────────────────
info "开始编译（release 模式）…"
cd "$AGENT_DIR"
cargo build --release 2>&1 | grep -E '(Compiling|Finished|error\[|^error)' || true

BINARY="$AGENT_DIR/target/release/any-kvm-agent"
[ -f "$BINARY" ] || die "编译失败，未找到 $BINARY"
ok "编译成功：$BINARY  ($(du -sh "$BINARY" | cut -f1))"

# ─── 4. 创建输出目录 & 预生成配置 ─────────────────────────────────────────────
mkdir -p "$DIST_DIR"
GENERATED_CONFIG="$DIST_DIR/config.toml"
_generate_config "$GENERATED_CONFIG"
ok "配置文件已生成：$GENERATED_CONFIG"

# ─── 5. 打包 tar.gz（通用，所有 Linux 发行版）────────────────────────────────
info "打包 tar.gz…"
TARBALL="$DIST_DIR/${PKG_NAME}.tar.gz"
TMP_TAR="$(mktemp -d)"
TAR_ROOT="$TMP_TAR/any-kvm-agent"
mkdir -p "$TAR_ROOT/bin" "$TAR_ROOT/etc" "$TAR_ROOT/systemd"

cp "$BINARY"               "$TAR_ROOT/bin/any-kvm-agent"
cp "$SCRIPT_DIR/any-kvm-agent-wrapper.sh" "$TAR_ROOT/bin/any-kvm-agent-wrapper"
chmod 755 "$TAR_ROOT/bin/any-kvm-agent-wrapper"
cp "$GENERATED_CONFIG"     "$TAR_ROOT/etc/config.toml"
cp "$AGENT_DIR/config.toml.example" "$TAR_ROOT/etc/config.toml.example"
if [ -f "$SCRIPT_DIR/any-kvm-agent.service" ]; then
    cp "$SCRIPT_DIR/any-kvm-agent.service" "$TAR_ROOT/systemd/any-kvm-agent.service"
else
    generate_service_file "$TAR_ROOT/systemd/any-kvm-agent.service"
fi

cat > "$TAR_ROOT/install.sh" << 'INSTALL_EOF'
#!/bin/bash
# Any-KVM Agent — tar.gz 安装脚本（通用 Linux）
set -e
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BOLD='\033[1m'; NC='\033[0m'
info() { echo -e "\033[0;34m[INFO]\033[0m $*"; }
ok()   { echo -e "${GREEN}[OK]\033[0m   $*"; }

info "安装 any-kvm-agent…"
sudo install -m 755 "$SCRIPT_DIR/bin/any-kvm-agent" /usr/bin/any-kvm-agent
sudo install -m 755 "$SCRIPT_DIR/bin/any-kvm-agent-wrapper" /usr/bin/any-kvm-agent-wrapper

sudo mkdir -p /etc/any-kvm-agent
if [ ! -f /etc/any-kvm-agent/config.toml ]; then
    sudo cp "$SCRIPT_DIR/etc/config.toml" /etc/any-kvm-agent/config.toml
    ok "配置文件已写入 /etc/any-kvm-agent/config.toml"
else
    info "配置文件已存在，保留不覆盖（备份见 config.toml.new）"
    sudo cp "$SCRIPT_DIR/etc/config.toml" /etc/any-kvm-agent/config.toml.new
fi
sudo cp "$SCRIPT_DIR/etc/config.toml.example" /etc/any-kvm-agent/config.toml.example

if command -v systemctl &>/dev/null; then
    sudo cp "$SCRIPT_DIR/systemd/any-kvm-agent.service" /etc/systemd/system/
    sudo systemctl daemon-reload
    sudo systemctl enable any-kvm-agent
    sudo systemctl restart any-kvm-agent
    ok "服务已启动并设为开机自启"
    sleep 1
    sudo systemctl status any-kvm-agent --no-pager -l || true
fi

echo ""
echo -e "${BOLD}安装完成！${NC}"
echo "  查看日志：journalctl -u any-kvm-agent -f"
echo "  修改配置：sudo nano /etc/any-kvm-agent/config.toml"
INSTALL_EOF
chmod +x "$TAR_ROOT/install.sh"

tar -czf "$TARBALL" -C "$TMP_TAR" any-kvm-agent
rm -rf "$TMP_TAR"
ok "tar.gz 已生成：$TARBALL"

# ─── 6. 打包 .deb（Debian / Ubuntu / Raspberry Pi OS）────────────────────────
if [ "$OPT_NO_DEB" = false ] && command -v dpkg-deb &>/dev/null; then
    info "打包 .deb…"
    DEB_FILE="$DIST_DIR/${PKG_NAME}.deb"
    TMP_DEB="$(mktemp -d)"

    install -d "$TMP_DEB/DEBIAN"
    install -d "$TMP_DEB/usr/bin"
    install -d "$TMP_DEB/etc/any-kvm-agent"
    install -d "$TMP_DEB/lib/systemd/system"
    install -d "$TMP_DEB/usr/share/doc/any-kvm-agent"

    install -m 755 "$BINARY"           "$TMP_DEB/usr/bin/any-kvm-agent"
    install -m 755 "$SCRIPT_DIR/any-kvm-agent-wrapper.sh" \
                                       "$TMP_DEB/usr/bin/any-kvm-agent-wrapper"
    install -m 644 "$GENERATED_CONFIG" "$TMP_DEB/etc/any-kvm-agent/config.toml"
    install -m 644 "$AGENT_DIR/config.toml.example" \
                                       "$TMP_DEB/etc/any-kvm-agent/config.toml.example"

    _svc="$TMP_DEB/lib/systemd/system/any-kvm-agent.service"
    if [ -f "$SCRIPT_DIR/any-kvm-agent.service" ]; then
        install -m 644 "$SCRIPT_DIR/any-kvm-agent.service" "$_svc"
    else
        write_service_file "$_svc"
    fi

    cat > "$TMP_DEB/DEBIAN/control" << EOF
Package: any-kvm-agent
Version: ${PKG_VERSION}
Architecture: ${DEB_ARCH}
Maintainer: Any-KVM Contributors
Depends: libasound2, libssl3 | libssl1.1
Recommends: coturn
Section: net
Priority: optional
Description: Any-KVM device-side agent
 Captures video (V4L2) and audio (ALSA), encodes to H.264/Opus,
 and streams via WebRTC P2P to a browser KVM console.
 Keyboard/mouse control is forwarded via USB HID Gadget or CH9329.
 Signal server: ${SIGNAL_URL}  Room: ${OPT_ROOM}
EOF

    # conffiles：标记 config.toml 为用户配置，升级时不覆盖
    echo "/etc/any-kvm-agent/config.toml" > "$TMP_DEB/DEBIAN/conffiles"

    # postinst：安装后启动服务
    cat > "$TMP_DEB/DEBIAN/postinst" << 'POSTINST_EOF'
#!/bin/bash
set -e
if command -v systemctl &>/dev/null && systemctl is-system-running --quiet 2>/dev/null; then
    systemctl daemon-reload
    systemctl enable any-kvm-agent.service || true
    systemctl restart any-kvm-agent.service || true
fi
echo ""
echo "╔══════════════════════════════════════════════════════════╗"
echo "║  Any-KVM Agent 已安装并启动！                            ║"
echo "║  查看状态：sudo systemctl status any-kvm-agent           ║"
echo "║  查看日志：journalctl -u any-kvm-agent -f                ║"
echo "║  修改配置：sudo nano /etc/any-kvm-agent/config.toml      ║"
echo "╚══════════════════════════════════════════════════════════╝"
POSTINST_EOF
    chmod 755 "$TMP_DEB/DEBIAN/postinst"

    # prerm：卸载前停止服务
    cat > "$TMP_DEB/DEBIAN/prerm" << 'PRERM_EOF'
#!/bin/bash
set -e
if command -v systemctl &>/dev/null; then
    systemctl stop    any-kvm-agent.service 2>/dev/null || true
    systemctl disable any-kvm-agent.service 2>/dev/null || true
fi
PRERM_EOF
    chmod 755 "$TMP_DEB/DEBIAN/prerm"

    cat > "$TMP_DEB/usr/share/doc/any-kvm-agent/copyright" << EOF
Any-KVM Agent — https://github.com/any-kvm/any-kvm
License: MIT
EOF

    dpkg-deb --build --root-owner-group "$TMP_DEB" "$DEB_FILE"
    rm -rf "$TMP_DEB"
    ok ".deb 已生成：$DEB_FILE  ($(du -sh "$DEB_FILE" | cut -f1))"
else
    [ "$OPT_NO_DEB" = false ] && warn "未找到 dpkg-deb，跳过 .deb 打包（仅生成 tar.gz）"
fi

# ─── 7. 可选：直接安装到本机 ─────────────────────────────────────────────────
if [ "$OPT_INSTALL" = true ]; then
    echo ""
    info "执行本地安装…"
    DEB_FILE="$DIST_DIR/${PKG_NAME}.deb"
    if [ -f "$DEB_FILE" ] && command -v dpkg &>/dev/null; then
        sudo dpkg -i "$DEB_FILE"
    else
        TMP_INST="$(mktemp -d)"
        tar -xzf "$TARBALL" -C "$TMP_INST"
        bash "$TMP_INST/any-kvm-agent/install.sh"
        rm -rf "$TMP_INST"
    fi
    echo ""
    info "服务状态："
    sudo systemctl status any-kvm-agent --no-pager -l 2>/dev/null || true
fi

# ─── 8. 完成摘要 ──────────────────────────────────────────────────────────────
echo ""
echo -e "${GREEN}${BOLD}═══════════════════════════ 完成 ═══════════════════════════${NC}"
echo -e "  信令服务器: ${BOLD}${SIGNAL_URL}${NC}"
echo -e "  设备房间ID: ${BOLD}${OPT_ROOM}${NC}"
echo ""
echo "输出文件："
ls -lh "$DIST_DIR"/ 2>/dev/null | grep -v '^total' | awk '{printf "  %-45s %s\n", $NF, $5}'
echo ""
echo "安装方法（Debian/Ubuntu/树莓派）："
echo -e "  ${BOLD}sudo dpkg -i $DIST_DIR/${PKG_NAME}.deb${NC}"
echo ""
echo "安装方法（通用 Linux）："
echo -e "  ${BOLD}tar -xzf $DIST_DIR/${PKG_NAME}.tar.gz && sudo ./any-kvm-agent/install.sh${NC}"
echo ""
echo "安装后服务将${BOLD}自动启动${NC}，服务器上打开浏览器："
echo -e "  ${BOLD}http://${OPT_SERVER}:${OPT_PORT}${NC}"

