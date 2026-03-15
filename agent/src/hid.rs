//! hid.rs — USB HID Gadget 输入注入 / CH9329 串口 HID
//!
//! 从 WebRTC DataChannel 收取 8 字节控制帧，解码后注入到被控设备：
//!   type=0x01 → 键盘报文 → /dev/hidg0
//!   type=0x02 → 鼠标移动（绝对坐标） → /dev/hidg1
//!   type=0x03 → 鼠标按键 → /dev/hidg1
//!   type=0x04 → 鼠标滚轮 → /dev/hidg1

use crate::config::HidConfig;
use anyhow::{bail, Result};
#[cfg(feature = "ch9329")]
use anyhow::Context;
use bytes::Bytes;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use tokio::sync::mpsc::Receiver;
use tracing::{debug, info, warn};

/// HID 设备状态标志位
pub const HID_STATUS_KEYBOARD: u8 = 0x01;
pub const HID_STATUS_MOUSE:    u8 = 0x02;

// ─── 帧类型常量 ───────────────────────────────────────────────────────────────
const TYPE_KEYBOARD:    u8 = 0x01;
const TYPE_MOUSE_MOVE:  u8 = 0x02;
const TYPE_MOUSE_BTN:   u8 = 0x03;
const TYPE_MOUSE_WHEEL: u8 = 0x04;
const TYPE_CONTROL:     u8 = 0x10;

/// Video control command sent via DataChannel
#[derive(Debug, Clone)]
pub enum VideoControl {
    ChangeResolution { width: u32, height: u32 },
    ChangeFps { fps: u32 },
}

pub fn run(cfg: HidConfig, rx: Receiver<Bytes>, video_ctrl_tx: Option<std::sync::mpsc::Sender<VideoControl>>, hid_status: Arc<AtomicU8>) -> Result<()> {
    info!("hid: mode={}", cfg.mode);

    match cfg.mode.as_str() {
        "gadget"  => run_gadget(cfg, rx, video_ctrl_tx, hid_status),
        #[cfg(feature = "ch9329")]
        "ch9329"  => run_ch9329(cfg, rx, video_ctrl_tx, hid_status),
        #[cfg(not(feature = "ch9329"))]
        "ch9329"  => bail!("ch9329 support not compiled in; rebuild with --features ch9329"),
        other     => bail!("unknown hid mode: '{}'", other),
    }
}

// ─── USB HID Gadget 模式 ──────────────────────────────────────────────────────

fn run_gadget(cfg: HidConfig, mut rx: Receiver<Bytes>, video_ctrl_tx: Option<std::sync::mpsc::Sender<VideoControl>>, hid_status: Arc<AtomicU8>) -> Result<()> {
    use std::io::Write;
    use std::fs::OpenOptions;

    let mut status: u8 = 0;

    // 打开 HID 设备为 Option — 失败只警告，不阻止控制消息处理
    let mut kbd = match OpenOptions::new().write(true).open(&cfg.keyboard_device) {
        Ok(f) => {
            info!("hid: keyboard gadget {:?} opened", cfg.keyboard_device);
            status |= HID_STATUS_KEYBOARD;
            Some(f)
        }
        Err(e) => {
            warn!("hid: keyboard gadget {:?} unavailable: {} (control messages still work)", cfg.keyboard_device, e);
            None
        }
    };
    let mut mouse = match OpenOptions::new().write(true).open(&cfg.mouse_device) {
        Ok(f) => {
            info!("hid: mouse gadget {:?} opened", cfg.mouse_device);
            status |= HID_STATUS_MOUSE;
            Some(f)
        }
        Err(e) => {
            warn!("hid: mouse gadget {:?} unavailable: {} (control messages still work)", cfg.mouse_device, e);
            None
        }
    };

    hid_status.store(status, Ordering::Relaxed);
    info!("hid: device status=0x{:02x} (kbd={}, mouse={})",
        status, kbd.is_some(), mouse.is_some());

    if kbd.is_some() && mouse.is_some() {
        info!("hid: USB Gadget mode fully operational");
    } else {
        warn!("hid: running in control-only mode (video control via DataChannel still active)");
    }

    let mut last_x: u16 = 0x3fff;
    let mut last_y: u16 = 0x3fff;
    let mut last_buttons: u8 = 0;

    while let Some(frame) = rx.blocking_recv() {
        if frame.len() < 8 {
            warn!("hid: short frame ({} bytes), skipping", frame.len());
            continue;
        }
        let ty = frame[0];
        match ty {
            TYPE_KEYBOARD => {
                if let Some(ref mut k) = kbd {
                    let report = [frame[1], 0x00, frame[2], frame[3],
                                  frame[4], frame[5], frame[6], frame[7]];
                    if let Err(e) = k.write_all(&report) {
                        warn!("hid: kbd write error: {e}");
                    }
                }
            }
            TYPE_MOUSE_MOVE => {
                last_buttons = frame[1];
                last_x = u16::from_be_bytes([frame[2], frame[3]]);
                last_y = u16::from_be_bytes([frame[4], frame[5]]);
                debug!("hid: mouse move → ({}, {})", last_x, last_y);
                if let Some(ref mut m) = mouse {
                    let report = make_abs_mouse_report(last_buttons, last_x, last_y, 0);
                    if let Err(e) = m.write_all(&report) {
                        warn!("hid: mouse write error: {e}");
                    }
                }
            }
            TYPE_MOUSE_BTN => {
                last_buttons = frame[1];
                if let Some(ref mut m) = mouse {
                    let report = make_abs_mouse_report(last_buttons, last_x, last_y, 0);
                    if let Err(e) = m.write_all(&report) {
                        warn!("hid: mouse btn write error: {e}");
                    }
                }
            }
            TYPE_MOUSE_WHEEL => {
                let delta = frame[1] as i8;
                if let Some(ref mut m) = mouse {
                    let report = make_abs_mouse_report(last_buttons, last_x, last_y, delta);
                    if let Err(e) = m.write_all(&report) {
                        warn!("hid: mouse wheel write error: {e}");
                    }
                }
            }
            TYPE_CONTROL => {
                if let Some(ref tx) = video_ctrl_tx {
                    match frame[1] {
                        0x01 => {
                            let w = u16::from_be_bytes([frame[2], frame[3]]) as u32;
                            let h = u16::from_be_bytes([frame[4], frame[5]]) as u32;
                            info!("hid: control → resolution change {}×{}", w, h);
                            let _ = tx.send(VideoControl::ChangeResolution { width: w, height: h });
                        }
                        0x02 => {
                            let fps = frame[2] as u32;
                            info!("hid: control → fps change {}", fps);
                            let _ = tx.send(VideoControl::ChangeFps { fps });
                        }
                        sub => debug!("hid: unknown control subtype 0x{:02x}", sub),
                    }
                }
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

#[cfg(feature = "ch9329")]
fn run_ch9329(cfg: HidConfig, mut rx: Receiver<Bytes>, video_ctrl_tx: Option<std::sync::mpsc::Sender<VideoControl>>, hid_status: Arc<AtomicU8>) -> Result<()> {
    use serialport::SerialPort;

    let mut port = serialport::new(&cfg.serial_port, cfg.serial_baud)
        .timeout(std::time::Duration::from_millis(50))
        .open()
        .with_context(|| format!("cannot open serial port '{}'", cfg.serial_port))?;

    // CH9329 模式：键盘和鼠标都通过串口，均视为可用
    hid_status.store(HID_STATUS_KEYBOARD | HID_STATUS_MOUSE, Ordering::Relaxed);
    info!("hid: CH9329 via {} @{}baud", cfg.serial_port, cfg.serial_baud);

    let mut last_x: u16 = 0x3fff;
    let mut last_y: u16 = 0x3fff;

    while let Some(frame) = rx.blocking_recv() {
        if frame.len() < 8 { continue; }
        let ty = frame[0];
        let packet = match ty {
            TYPE_KEYBOARD => {
                ch9329_keyboard(frame[1], &frame[2..8])
            }
            TYPE_MOUSE_MOVE => {
                last_x = u16::from_be_bytes([frame[2], frame[3]]);
                last_y = u16::from_be_bytes([frame[4], frame[5]]);
                ch9329_abs_mouse(frame[1], last_x, last_y)
            }
            TYPE_MOUSE_BTN => {
                ch9329_abs_mouse(frame[1], last_x, last_y)
            }
            TYPE_MOUSE_WHEEL => {
                // CH9329 wheel not directly supported in abs mouse command
                ch9329_abs_mouse(0, last_x, last_y)
            }
            TYPE_CONTROL => {
                if let Some(ref tx) = video_ctrl_tx {
                    match frame[1] {
                        0x01 => {
                            let w = u16::from_be_bytes([frame[2], frame[3]]) as u32;
                            let h = u16::from_be_bytes([frame[4], frame[5]]) as u32;
                            info!("hid: control → resolution change {}×{}", w, h);
                            let _ = tx.send(VideoControl::ChangeResolution { width: w, height: h });
                        }
                        0x02 => {
                            let fps = frame[2] as u32;
                            info!("hid: control → fps change {}", fps);
                            let _ = tx.send(VideoControl::ChangeFps { fps });
                        }
                        _ => {}
                    }
                }
                continue;
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
#[cfg(feature = "ch9329")]fn ch9329_frame(cmd: u8, data: &[u8]) -> Vec<u8> {
    let mut frame = vec![0x57u8, 0xAB, 0x00, cmd, data.len() as u8];
    frame.extend_from_slice(data);
    let sum: u32 = frame.iter().map(|&b| b as u32).sum();
    frame.push((sum & 0xff) as u8);
    frame
}

#[cfg(feature = "ch9329")]
fn ch9329_keyboard(modifier: u8, keys: &[u8]) -> Vec<u8> {
    // CMD 0x02: HID 键盘
    let data = [modifier, 0x00, keys[0], keys[1], keys[2], keys[3], keys[4], keys[5]];
    ch9329_frame(0x02, &data)
}

#[cfg(feature = "ch9329")]
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
