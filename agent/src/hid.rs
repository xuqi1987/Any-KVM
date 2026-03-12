//! hid.rs — USB HID Gadget 输入注入 / CH9329 串口 HID
//!
//! 从 WebRTC DataChannel 收取 8 字节控制帧，解码后注入到被控设备：
//!   type=0x01 → 键盘报文 → /dev/hidg0
//!   type=0x02 → 鼠标移动（绝对坐标） → /dev/hidg1
//!   type=0x03 → 鼠标按键 → /dev/hidg1
//!   type=0x04 → 鼠标滚轮 → /dev/hidg1

use crate::config::HidConfig;
use anyhow::{bail, Context, Result};
use bytes::Bytes;
use tokio::sync::mpsc::Receiver;
use tracing::{debug, info, warn};

// ─── 帧类型常量 ───────────────────────────────────────────────────────────────
const TYPE_KEYBOARD:    u8 = 0x01;
const TYPE_MOUSE_MOVE:  u8 = 0x02;
const TYPE_MOUSE_BTN:   u8 = 0x03;
const TYPE_MOUSE_WHEEL: u8 = 0x04;

pub fn run(cfg: HidConfig, mut rx: Receiver<Bytes>) -> Result<()> {
    info!("hid: mode={}", cfg.mode);

    match cfg.mode.as_str() {
        "gadget"  => run_gadget(cfg, rx),
        "ch9329"  => run_ch9329(cfg, rx),
        other     => bail!("unknown hid mode: '{}'", other),
    }
}

// ─── USB HID Gadget 模式 ──────────────────────────────────────────────────────

fn run_gadget(cfg: HidConfig, mut rx: Receiver<Bytes>) -> Result<()> {
    use std::io::Write;
    use std::fs::OpenOptions;

    let mut kbd = OpenOptions::new().write(true)
        .open(&cfg.keyboard_device)
        .with_context(|| format!("cannot open keyboard gadget {:?}", cfg.keyboard_device))?;
    let mut mouse = OpenOptions::new().write(true)
        .open(&cfg.mouse_device)
        .with_context(|| format!("cannot open mouse gadget {:?}", cfg.mouse_device))?;

    info!("hid: USB Gadget keyboard={:?} mouse={:?}", cfg.keyboard_device, cfg.mouse_device);

    while let Some(frame) = rx.blocking_recv() {
        if frame.len() < 8 {
            warn!("hid: short frame ({} bytes), skipping", frame.len());
            continue;
        }
        let ty = frame[0];
        match ty {
            TYPE_KEYBOARD => {
                // USB HID Boot Keyboard 报文：8 字节
                // [modifier, reserved, key1..key6]
                let report = [frame[1], 0x00, frame[2], frame[3],
                              frame[4], frame[5], frame[6], frame[7]];
                kbd.write_all(&report).context("kbd write error")?;
            }
            TYPE_MOUSE_MOVE => {
                // 绝对鼠标报文（需 USB Descriptor 为 Absolute Mouse）
                // [buttons, abs_x_hi, abs_x_lo, abs_y_hi, abs_y_lo, wheel]
                let buttons = frame[1];
                let ax = u16::from_be_bytes([frame[2], frame[3]]);
                let ay = u16::from_be_bytes([frame[4], frame[5]]);
                let report = make_abs_mouse_report(buttons, ax, ay, 0);
                mouse.write_all(&report).context("mouse write error")?;
            }
            TYPE_MOUSE_BTN => {
                let buttons = frame[1];
                let report = make_abs_mouse_report(buttons, 0x3fff, 0x3fff, 0);
                mouse.write_all(&report).context("mouse btn write error")?;
            }
            TYPE_MOUSE_WHEEL => {
                let delta = frame[1] as i8;
                let report = make_abs_mouse_report(0, 0x3fff, 0x3fff, delta);
                mouse.write_all(&report).context("mouse wheel write error")?;
            }
            other => debug!("hid: unknown frame type 0x{:02x}", other),
        }
    }
    Ok(())
}

/// 构建绝对坐标鼠标 HID 报文（6 字节）
/// Descriptor: buttons(1) + x(2, absolute 0-32767) + y(2) + wheel(1)
fn make_abs_mouse_report(buttons: u8, x: u16, y: u16, wheel: i8) -> [u8; 6] {
    [
        buttons,
        (x & 0xff) as u8,
        ((x >> 8) & 0xff) as u8,
        (y & 0xff) as u8,
        ((y >> 8) & 0xff) as u8,
        wheel as u8,
    ]
}

// ─── CH9329 串口 HID 模式（回退方案）─────────────────────────────────────────

fn run_ch9329(cfg: HidConfig, mut rx: Receiver<Bytes>) -> Result<()> {
    use serialport::SerialPort;

    let mut port = serialport::new(&cfg.serial_port, cfg.serial_baud)
        .timeout(std::time::Duration::from_millis(50))
        .open()
        .with_context(|| format!("cannot open serial port '{}'", cfg.serial_port))?;

    info!("hid: CH9329 via {} @{}baud", cfg.serial_port, cfg.serial_baud);

    while let Some(frame) = rx.blocking_recv() {
        if frame.len() < 8 { continue; }
        let ty = frame[0];
        let packet = match ty {
            TYPE_KEYBOARD => {
                // CH9329 键盘协议帧
                ch9329_keyboard(frame[1], &frame[2..8])
            }
            TYPE_MOUSE_MOVE | TYPE_MOUSE_BTN | TYPE_MOUSE_WHEEL => {
                // CH9329 绝对鼠标协议帧
                let buttons = if ty == TYPE_MOUSE_BTN { frame[1] } else { 0 };
                let ax = if ty == TYPE_MOUSE_MOVE {
                    u16::from_be_bytes([frame[2], frame[3]])
                } else { 0x3fff };
                let ay = if ty == TYPE_MOUSE_MOVE {
                    u16::from_be_bytes([frame[4], frame[5]])
                } else { 0x3fff };
                ch9329_abs_mouse(buttons, ax, ay)
            }
            _ => continue,
        };
        if let Err(e) = port.write_all(&packet) {
            warn!("ch9329 write error: {}", e);
        }
    }
    Ok(())
}

// ─── CH9329 协议帧构造 ────────────────────────────────────────────────────────
// 帧格式: [0x57, 0xAB, addr(0x00), cmd, len, data..., checksum]

fn ch9329_frame(cmd: u8, data: &[u8]) -> Vec<u8> {
    let mut frame = vec![0x57u8, 0xAB, 0x00, cmd, data.len() as u8];
    frame.extend_from_slice(data);
    let sum: u32 = frame.iter().map(|&b| b as u32).sum();
    frame.push((sum & 0xff) as u8);
    frame
}

fn ch9329_keyboard(modifier: u8, keys: &[u8]) -> Vec<u8> {
    // CMD 0x02: HID 键盘
    let data = [modifier, 0x00, keys[0], keys[1], keys[2], keys[3], keys[4], keys[5]];
    ch9329_frame(0x02, &data)
}

fn ch9329_abs_mouse(buttons: u8, x: u16, y: u16) -> Vec<u8> {
    // CMD 0x04: HID 绝对鼠标
    let data = [
        buttons,
        (x & 0xff) as u8, ((x >> 8) & 0xff) as u8,
        (y & 0xff) as u8, ((y >> 8) & 0xff) as u8,
        0x00, // wheel
    ];
    ch9329_frame(0x04, &data)
}
