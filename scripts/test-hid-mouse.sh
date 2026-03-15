#!/bin/bash
# 玩客云鼠标 HID 设备测试脚本
# 用于诊断和测试鼠标输入是否正常工作

set -e

HIDG1="/dev/hidg1"

echo "=== Any-KVM 鼠标 HID 测试 ==="
echo

# 1. 检查设备存在
echo "[1/5] 检查 HID 设备..."
if [ ! -e "$HIDG1" ]; then
    echo "❌ 错误: $HIDG1 不存在"
    echo "   请先运行 scripts/setup-hid-gadget.sh 配置 USB Gadget"
    exit 1
fi
echo "✓ $HIDG1 存在"

# 2. 检查权限
echo
echo "[2/5] 检查设备权限..."
if [ ! -w "$HIDG1" ]; then
    echo "⚠ 警告: $HIDG1 不可写，尝试修改权限..."
    sudo chmod 666 "$HIDG1" || {
        echo "❌ 错误: 无法修改权限"
        exit 1
    }
fi
echo "✓ 设备可写"

# 3. 检查 USB 连接状态
echo
echo "[3/5] 检查 USB Gadget 状态..."
UDC_STATE=$(cat /sys/class/udc/*/state 2>/dev/null || echo "unknown")
echo "   UDC 状态: $UDC_STATE"
if [ "$UDC_STATE" != "configured" ]; then
    echo "⚠ 警告: USB Gadget 状态不是 'configured'"
    echo "   这可能导致鼠标输入无法传递到目标主机"
    echo "   请检查:"
    echo "   1. USB OTG 线缆是否连接到目标主机"
    echo "   2. 目标主机是否识别到 USB 设备"
fi

# 4. 测试写入
echo
echo "[4/5] 测试鼠标报文写入..."
# 鼠标报文格式: [buttons(1), x_lo(1), x_hi(1), y_lo(1), y_hi(1), wheel(1), 0, 0]
# 无操作: 所有字段为 0
echo -ne '\x00\x00\x00\x00\x00\x00\x00\x00' > "$HIDG1" && echo "✓ 写入成功" || {
    echo "❌ 写入失败"
    echo "   错误可能原因:"
    echo "   - USB 端点已关闭 (errno 108: endpoint shutdown)"
    echo "   - 目标主机未连接或未识别设备"
    exit 1
}

# 5. 执行移动测试
echo
echo "[5/5] 执行鼠标移动测试..."
echo "   即将发送 5 个鼠标移动报文（向右下角移动）"
echo "   如果目标主机已连接，您应该能看到鼠标指针移动"
echo "   按 Enter 继续..."
read

for i in {1..5}; do
    # 向右移动 50 像素，向下移动 50 像素
    # X = 50, Y = 50 (绝对坐标模式下需要转换为 0-32767 范围)
    # 这里使用相对移动: [buttons, dx, dy, wheel]
    # 注意: 当前 HID 使用绝对坐标，这里仅作为测试
    echo -ne '\x00\x32\x00\x32\x00\x00\x00\x00' > "$HIDG1"
    echo "   发送移动 #$i"
    sleep 0.2
done

echo
echo "✅ 测试完成"
echo
echo "如果鼠标没有移动，请检查:"
echo "1. 目标主机是否已连接 USB OTG 线缆"
echo "2. 目标主机上是否识别到 'KVM Composite Device'"
echo "   - Linux: lsusb | grep -i kvm"
echo "   - Windows: 设备管理器 → 人体学输入设备"
echo "3. Web 页面是否显示鼠标状态为可用"
echo "4. Agent 日志中是否有 'mouse write error'"
echo "   - journalctl -u any-kvm-agent -f"
echo
echo "调试命令:"
echo "  查看 USB 连接日志: dmesg | grep -E 'usb|dwc2|gadget' | tail -20"
echo "  查看 Agent 日志:   journalctl -u any-kvm-agent --since '5 min ago'"
echo "  重新配置 Gadget:   bash scripts/setup-hid-gadget.sh"
