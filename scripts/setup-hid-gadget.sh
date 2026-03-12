#!/bin/bash
# setup-hid-gadget.sh
# 在玩客云（或任何支持 USB OTG Gadget 的 Linux 设备）上配置 USB HID Gadget
# 必须以 root 身份运行，且内核已加载 libcomposite 模块
#
# 配置完成后：
#   /dev/hidg0 → USB 键盘（Boot Keyboard）
#   /dev/hidg1 → USB 鼠标（绝对坐标 Absolute Mouse）
#
# 用法: sudo bash setup-hid-gadget.sh

set -euo pipefail

GADGET_DIR="/sys/kernel/config/usb_gadget/any-kvm"
UDC=$(ls /sys/class/udc | head -n1)

if [[ -z "$UDC" ]]; then
    echo "ERROR: No USB Device Controller found. Check OTG support." >&2
    exit 1
fi

echo "Using UDC: $UDC"

# 加载必要内核模块
modprobe libcomposite

# ─── 创建 Gadget 根目录 ────────────────────────────────────────────────────────
mkdir -p "$GADGET_DIR"
cd "$GADGET_DIR"

# USB 设备描述符
echo 0x1d6b > idVendor    # Linux Foundation
echo 0x0104 > idProduct   # Multifunction Composite Gadget
echo 0x0100 > bcdDevice
echo 0x0200 > bcdUSB

# 设备字符串
mkdir -p strings/0x409
echo "AnyKVM"         > strings/0x409/manufacturer
echo "Any-KVM HID"    > strings/0x409/product
echo "anykvm001"      > strings/0x409/serialnumber

# ─── 配置 1 ───────────────────────────────────────────────────────────────────
mkdir -p configs/c.1/strings/0x409
echo "HID Config" > configs/c.1/strings/0x409/configuration
echo 120          > configs/c.1/MaxPower   # 120mA

# ─── 功能 1: USB 键盘（Boot Keyboard Descriptor）──────────────────────────────
mkdir -p functions/hid.keyboard
echo 1    > functions/hid.keyboard/protocol   # Keyboard
echo 1    > functions/hid.keyboard/subclass   # Boot Interface
echo 8    > functions/hid.keyboard/report_length

# HID Boot Keyboard Report Descriptor
printf '\x05\x01\x09\x06\xa1\x01\x05\x07\x19\xe0\x29\xe7\x15\x00\x25\x01\x75\x01\x95\x08\x81\x02\x95\x01\x75\x08\x81\x03\x95\x05\x75\x01\x05\x08\x19\x01\x29\x05\x91\x02\x95\x01\x75\x03\x91\x03\x95\x06\x75\x08\x15\x00\x25\x65\x05\x07\x19\x00\x29\x65\x81\x00\xc0' \
    > functions/hid.keyboard/report_desc

# ─── 功能 2: USB 绝对坐标鼠标 ──────────────────────────────────────────────────
mkdir -p functions/hid.mouse
echo 0    > functions/hid.mouse/protocol      # None (Not boot)
echo 0    > functions/hid.mouse/subclass
echo 6    > functions/hid.mouse/report_length

# Absolute Mouse Report Descriptor（6 字节：buttons, x*2, y*2, wheel）
printf '\x05\x01\x09\x02\xa1\x01\x09\x01\xa1\x00\x05\x09\x19\x01\x29\x03\x15\x00\x25\x01\x95\x03\x75\x01\x81\x02\x95\x01\x75\x05\x81\x03\x05\x01\x09\x30\x09\x31\x15\x00\x26\xff\x7f\x75\x10\x95\x02\x81\x02\x09\x38\x15\x81\x25\x7f\x75\x08\x95\x01\x81\x06\xc0\xc0' \
    > functions/hid.mouse/report_desc

# ─── 链接功能到配置 ────────────────────────────────────────────────────────────
ln -s "$GADGET_DIR/functions/hid.keyboard" configs/c.1/
ln -s "$GADGET_DIR/functions/hid.mouse"    configs/c.1/

# ─── 激活 Gadget ───────────────────────────────────────────────────────────────
echo "$UDC" > UDC

echo ""
echo "✅ USB HID Gadget configured successfully."
echo "   Keyboard: $(ls /dev/hidg0 2>/dev/null || echo 'not found')"
echo "   Mouse:    $(ls /dev/hidg1 2>/dev/null || echo 'not found')"

# 设置设备节点权限（允许 any-kvm-agent 非 root 读写）
sleep 1
chmod 0660 /dev/hidg0 /dev/hidg1 2>/dev/null || true
chown root:input /dev/hidg0 /dev/hidg1 2>/dev/null || true
