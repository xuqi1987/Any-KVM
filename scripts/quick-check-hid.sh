#!/bin/bash
# quick-check-hid.sh — 快速检查 HID Gadget 状态并尝试修复
# 用法: bash quick-check-hid.sh

set -euo pipefail

echo "=== Any-KVM HID Gadget 状态检查 ==="
echo

# 1. 检查 HID 设备节点
echo "1. 检查 HID 设备节点..."
if [[ -c /dev/hidg0 ]] && [[ -c /dev/hidg1 ]]; then
    echo "   ✅ /dev/hidg0 (keyboard) 存在"
    echo "   ✅ /dev/hidg1 (mouse) 存在"
    HID_EXISTS=true
else
    echo "   ❌ HID 设备不存在"
    HID_EXISTS=false
fi
echo

# 2. 检查 USB Device Controller
echo "2. 检查 USB Device Controller..."
if ls /sys/class/udc/* &>/dev/null; then
    UDC=$(ls /sys/class/udc | head -n1)
    echo "   ✅ UDC 可用: $UDC"
    UDC_OK=true
else
    echo "   ❌ 未找到 UDC（设备不支持 USB OTG Gadget）"
    UDC_OK=false
fi
echo

# 3. 检查 libcomposite 模块
echo "3. 检查 libcomposite 模块..."
if lsmod | grep -q libcomposite; then
    echo "   ✅ libcomposite 已加载"
elif modprobe libcomposite &>/dev/null; then
    echo "   ✅ libcomposite 加载成功"
else
    echo "   ❌ libcomposite 不可用"
fi
echo

# 4. 检查 any-kvm-agent 服务
echo "4. 检查 any-kvm-agent 服务..."
if systemctl is-active any-kvm-agent &>/dev/null; then
    echo "   ✅ Agent 服务运行中"
    echo "   最近日志:"
    journalctl -u any-kvm-agent -n 5 --no-pager | tail -5 | sed 's/^/      /'
else
    echo "   ❌ Agent 服务未运行"
fi
echo

# 5. 汇总和建议
echo "=== 诊断结果 ==="
if $HID_EXISTS; then
    echo "✅ HID Gadget 已配置，无需操作"
    echo "   如果键鼠仍不可用，请检查 agent 日志："
    echo "   journalctl -u any-kvm-agent -f"
elif $UDC_OK; then
    echo "⚠️  HID Gadget 未配置，但硬件支持"
    echo
    echo "修复命令:"
    echo "   cd /root/Any-KVM"
    echo "   bash scripts/setup-hid-gadget.sh"
    echo "   systemctl restart any-kvm-agent"
    echo
    read -p "是否现在执行配置? [y/N] " -n 1 -r
    echo
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        echo "正在配置..."
        bash "$(dirname "$0")/setup-hid-gadget.sh"
        echo "正在重启 agent..."
        systemctl restart any-kvm-agent
        sleep 2
        echo
        echo "=== 配置完成 ==="
        echo "请刷新浏览器页面，重新连接到设备"
        journalctl -u any-kvm-agent -n 10 --no-pager | grep -i hid
    fi
else
    echo "❌ 硬件不支持 USB OTG Gadget"
    echo "   可能的原因:"
    echo "   1. 设备不是玩客云或不支持 OTG"
    echo "   2. 内核缺少 dwc2/libcomposite 支持"
    echo "   3. 需要重新刷入支持 OTG 的 Armbian 镜像"
    echo
    echo "   替代方案: 使用 CH9329 USB-HID 芯片（需硬件外接）"
fi
