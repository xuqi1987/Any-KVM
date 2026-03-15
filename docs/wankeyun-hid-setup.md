# 玩客云 USB HID Gadget 配置指南

## 问题症状

- 浏览器能连接到 wankeyun 并看到视频画面
- 状态栏 HID 指示器显示 "⌨❌ 🖱❌"（红色）
- 键盘和鼠标输入无响应

## 根本原因

玩客云上的 `/dev/hidg0`（键盘）和 `/dev/hidg1`（鼠标）设备节点不存在，因为 USB HID Gadget 未配置。

## 解决步骤

### 1. SSH 登录到玩客云

```bash
ssh root@192.168.31.132
```

### 2. 检查当前状态

```bash
# 检查 agent 是否运行
systemctl status any-kvm-agent

# 查看 agent 日志（应该看到 "keyboard gadget unavailable" 警告）
journalctl -u any-kvm-agent -n 50 | grep -i hid

# 检查 HID 设备是否存在
ls -l /dev/hidg*
# 如果显示 "No such file or directory"，继续下一步
```

### 3. 检查 USB Device Controller

```bash
# 确认玩客云支持 USB OTG Gadget
ls /sys/class/udc
# 应该显示类似 "c9100000.usb" 的控制器名称
```

### 4. 执行 HID Gadget 配置脚本

```bash
cd /root/Any-KVM
bash scripts/setup-hid-gadget.sh
```

**预期输出**：
```
Using UDC: c9100000.usb
Successfully configured USB HID Gadget
  /dev/hidg0 → Keyboard
  /dev/hidg1 → Mouse (Absolute)
```

### 5. 验证设备节点

```bash
ls -l /dev/hidg*
# 应该显示：
# crw------- 1 root root 243, 0 Mar 15 18:50 /dev/hidg0
# crw------- 1 root root 243, 1 Mar 15 18:50 /dev/hidg1
```

### 6. 重启 agent

```bash
systemctl restart any-kvm-agent
journalctl -u any-kvm-agent -f
```

**预期日志**：
```
hid: keyboard gadget "/dev/hidg0" opened
hid: mouse gadget "/dev/hidg1" opened
hid: device status=0x03 (kbd=true, mouse=true)
hid: USB Gadget mode fully operational
```

### 7. 浏览器验证

1. 刷新浏览器页面（Ctrl+Shift+R 强制刷新）
2. 重新连接到 wankeyun
3. 状态栏应显示 "⌨✅ 🖱✅"（绿色）
4. 测试键盘输入和鼠标移动

## 开机自动加载（可选）

### 方法 1：systemd service（推荐）

创建 `/etc/systemd/system/usb-hid-gadget.service`：

```ini
[Unit]
Description=USB HID Gadget for Any-KVM
After=sys-kernel-config.mount
Before=any-kvm-agent.service

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/root/Any-KVM/scripts/setup-hid-gadget.sh

[Install]
WantedBy=multi-user.target
```

启用服务：
```bash
systemctl daemon-reload
systemctl enable usb-hid-gadget
systemctl start usb-hid-gadget
```

### 方法 2：rc.local

编辑 `/etc/rc.local`（如果不存在则创建）：

```bash
#!/bin/bash
# USB HID Gadget setup
/root/Any-KVM/scripts/setup-hid-gadget.sh
exit 0
```

设置执行权限：
```bash
chmod +x /etc/rc.local
```

## 故障排查

### Q: setup-hid-gadget.sh 报错 "No USB Device Controller found"

**A**: 玩客云不支持 USB OTG Gadget，或内核缺少 `libcomposite` 模块。

解决方案：
```bash
# 检查内核模块
lsmod | grep libcomposite

# 如果没有，尝试加载
modprobe libcomposite

# 如果仍然失败，可能需要重新编译内核或使用支持 OTG 的 Armbian 镜像
```

### Q: /dev/hidg0 创建后 agent 仍无法打开

**A**: 检查权限

```bash
ls -l /dev/hidg*
# 确保 root 可写

# 如果需要，修改权限
chmod 600 /dev/hidg*
```

### Q: 键盘输入延迟或丢失

**A**: 这可能是 CPU 负载过高。检查：

```bash
top
# 如果 any-kvm-agent 占用 >80% CPU，考虑降低分辨率或帧率
```

### Q: 鼠标移动不准确

**A**: 确认使用绝对坐标模式（已配置在 setup-hid-gadget.sh 中）。如果仍有问题，检查 agent 日志看是否有鼠标坐标范围错误。

## 技术细节

### HID Report Descriptors

**键盘**（Boot Keyboard Protocol）：
- 8 字节：[Modifier, Reserved, Key1, Key2, Key3, Key4, Key5, Key6]
- 支持同时按下 6 个普通键 + 修饰键

**鼠标**（绝对坐标）：
- 6 字节：[Buttons, X_lo, X_hi, Y_lo, Y_hi, Wheel]
- X/Y 范围：0-32767（16位无符号）
- Buttons: bit0=左键, bit1=右键, bit2=中键

这些格式与 agent 代码（`agent/src/hid.rs`）中的帧格式完全匹配。

## 参考资料

- [Linux USB Gadget configfs](https://www.kernel.org/doc/html/latest/usb/gadget_configfs.html)
- [HID Usage Tables](https://www.usb.org/sites/default/files/hut1_2.pdf)
- agent/src/hid.rs — Agent HID 模块源码
- scripts/setup-hid-gadget.sh — 配置脚本源码
