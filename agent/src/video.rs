//! video.rs — 视频采集 + H.264 编码
//!
//! 支持两种视频源（由 config.video.source 控制）：
//!
//! * `"v4l2"`（默认）— USB 视频采集卡 / 摄像头
//!     流程：打开 V4L2 设备 → 优先协商 H264 硬编 → 回退 YUYV + openh264 软编
//!
//! * `"screen"` — 本机桌面截图（需编译 feature: screen-capture）
//!     流程：scrap 抓取 BGRA → 转 YUV420 → openh264 软编
//!     要求：X11 或 Wayland + wlr-screencopy；无显示环境不可用

use crate::config::VideoConfig;
use anyhow::{bail, Context, Result};
use bytes::Bytes;
use tokio::sync::mpsc::Sender;
use tracing::{info, warn};

// V4L2 相关 import（仅 v4l2-capture 路径使用）
#[cfg(feature = "v4l2-capture")]
use v4l::buffer::Type;
#[cfg(feature = "v4l2-capture")]
use v4l::io::traits::CaptureStream;
#[cfg(feature = "v4l2-capture")]
use v4l::video::Capture;
#[cfg(feature = "v4l2-capture")]
use v4l::{Device, FourCC};

pub fn run(cfg: VideoConfig, tx: Sender<Bytes>) -> Result<()> {
    match cfg.source.as_str() {
        "screen" => {
            #[cfg(feature = "screen-capture")]
            {
                info!("video: source=screen (desktop capture)");
                return run_screen_capture(&cfg, &tx);
            }
            #[cfg(not(feature = "screen-capture"))]
            bail!(
                "video source \"screen\" requires the 'screen-capture' feature; \
                 rebuild with: cargo build --release --features screen-capture"
            );
        }
        _ => {
            #[cfg(feature = "v4l2-capture")]
            {
                info!(
                    "video: source=v4l2  opening {} ({}×{}@{}fps, target {}kbps)",
                    cfg.device.display(), cfg.width, cfg.height, cfg.fps, cfg.bitrate_kbps
                );
                let dev = Device::with_path(&cfg.device)
                    .with_context(|| format!("cannot open V4L2 device {:?}", cfg.device))?;

                let hw_ok = cfg.hw_encode && try_hw_h264(&dev, &cfg, &tx).is_ok();
                if !hw_ok {
                    warn!("video: hardware H.264 not available, falling back to openh264");
                    run_sw_h264(&dev, &cfg, &tx)?;
                }
            }
            #[cfg(not(feature = "v4l2-capture"))]
            bail!(
                "video source \"v4l2\" requires the 'v4l2-capture' feature; \
                 rebuild with: cargo build --release --features v4l2-capture"
            );
        }
    }
    #[allow(unreachable_code)]
    Ok(())
}

// ─── 硬件 H.264（V4L2 直接输出 H.264，典型于 Amlogic / Rockchip）────────────

#[cfg(feature = "v4l2-capture")]
fn try_hw_h264(dev: &Device, cfg: &VideoConfig, tx: &Sender<Bytes>) -> Result<()> {
    let mut fmt = dev.format()?;
    fmt.width  = cfg.width;
    fmt.height = cfg.height;
    fmt.fourcc = FourCC::new(b"H264");
    let actual = dev.set_format(&fmt)?;

    if actual.fourcc != FourCC::new(b"H264") {
        bail!("device does not support H264 output format");
    }

    let mut params = dev.params()?;
    params.interval = v4l::Fraction { numerator: 1, denominator: cfg.fps };
    dev.set_params(&params)?;

    info!("video: HW H264 {}×{} @{}fps", actual.width, actual.height, cfg.fps);

    let mut stream = v4l::io::mmap::Stream::with_buffers(dev, Type::VideoCapture, 4)
        .context("failed to create V4L2 mmap stream")?;

    loop {
        let (buf, _meta) = stream.next()?;
        if tx.blocking_send(Bytes::copy_from_slice(buf)).is_err() {
            break;
        }
    }
    Ok(())
}

// ─── 软件 H.264（YUYV → openh264）──────────────────────────────────────────

#[cfg(feature = "v4l2-capture")]
fn run_sw_h264(dev: &Device, cfg: &VideoConfig, tx: &Sender<Bytes>) -> Result<()> {
    let mut fmt = dev.format()?;
    fmt.width  = cfg.width;
    fmt.height = cfg.height;
    fmt.fourcc = FourCC::new(b"YUYV");
    let actual = dev.set_format(&fmt)?;
    if actual.fourcc != FourCC::new(b"YUYV") {
        bail!("device does not support YUYV for software encoding");
    }

    let mut params = dev.params()?;
    params.interval = v4l::Fraction { numerator: 1, denominator: cfg.fps };
    dev.set_params(&params)?;

    info!("video: SW H264 {}×{} @{}fps via openh264", cfg.width, cfg.height, cfg.fps);

    let api = openh264::OpenH264API::from_source();
    let enc_cfg = openh264::encoder::EncoderConfig::new()
        .set_bitrate_bps(cfg.bitrate_kbps * 1000)
        .max_frame_rate(cfg.fps as f32)
        .debug(false);
    let mut encoder = openh264::encoder::Encoder::with_api_config(api, enc_cfg)
        .context("failed to init openh264 encoder")?;

    let w = cfg.width as usize;
    let h = cfg.height as usize;
    let mut stream = v4l::io::mmap::Stream::with_buffers(dev, Type::VideoCapture, 4)?;

    loop {
        let (buf, _meta) = stream.next()?;
        let yuv = yuyv_to_yuv420(buf, w, h);
        let src = openh264::formats::YUVBuffer::from_vec(yuv, w, h);
        let bitstream = encoder.encode(&src).context("openh264 encode error")?;
        let encoded = Bytes::from(bitstream.to_vec());
        if !encoded.is_empty() && tx.blocking_send(encoded).is_err() {
            return Ok(());
        }
    }
}

// ─── 屏幕截图源（feature: screen-capture）────────────────────────────────────

#[cfg(feature = "screen-capture")]
fn run_screen_capture(cfg: &VideoConfig, tx: &Sender<Bytes>) -> Result<()> {
    use scrap::{Capturer, Display};
    use std::thread;
    use std::time::{Duration, Instant};

    let display = Display::primary().context("failed to get primary display (is DISPLAY set?)")?;
    let (disp_w, disp_h) = (display.width(), display.height());
    let mut capturer = Capturer::new(display).context("failed to create screen capturer")?;

    // 优先使用配置中的分辨率，若为 0 则用显示器原生分辨率
    let w = if cfg.width  > 0 { cfg.width  as usize } else { disp_w };
    let h = if cfg.height > 0 { cfg.height as usize } else { disp_h };
    // scrap 总是按原生分辨率抓图，编码目标分辨率以 w×h 为准
    let enc_w = w;
    let enc_h = h;

    info!(
        "video: screen capture {}×{} (display native {}×{}) @{}fps {}kbps",
        enc_w, enc_h, disp_w, disp_h, cfg.fps, cfg.bitrate_kbps
    );

    let api = openh264::OpenH264API::from_source();
    let enc_cfg = openh264::encoder::EncoderConfig::new()
        .set_bitrate_bps(cfg.bitrate_kbps * 1000)
        .max_frame_rate(cfg.fps as f32)
        .debug(false);
    let mut encoder = openh264::encoder::Encoder::with_api_config(api, enc_cfg)
        .context("failed to init openh264 encoder")?;

    let frame_interval = Duration::from_nanos(1_000_000_000 / cfg.fps as u64);
    let mut next_frame = Instant::now();

    loop {
        match capturer.frame() {
            Ok(raw) => {
                // raw: BGRA，stride = raw.len() / disp_h
                let stride = raw.len() / disp_h;
                let yuv = bgra_to_yuv420(&raw, stride, disp_w, disp_h, enc_w, enc_h);
                let src = openh264::formats::YUVBuffer::from_vec(yuv, enc_w, enc_h);
                let bitstream = encoder.encode(&src).context("openh264 encode error")?;
                let encoded = Bytes::from(bitstream.to_vec());
                if !encoded.is_empty() && tx.blocking_send(encoded).is_err() {
                    return Ok(());
                }
                // 限速到目标帧率
                next_frame += frame_interval;
                let now = Instant::now();
                if next_frame > now {
                    thread::sleep(next_frame - now);
                } else {
                    next_frame = now; // 落后时重置，避免追帧
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // 新帧还未就绪，稍等
                thread::sleep(Duration::from_millis(1));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// BGRA → YUV420 平面（BT.601 limited range）
/// src_w/src_h：原图尺寸（scrap 抓取的分辨率）
/// dst_w/dst_h：输出编码尺寸（若与原图不同则做最近邻缩放）
#[cfg(feature = "screen-capture")]
fn bgra_to_yuv420(
    frame: &[u8], stride: usize,
    src_w: usize, src_h: usize,
    dst_w: usize, dst_h: usize,
) -> Vec<u8> {
    let frame_size = dst_w * dst_h;
    let mut yuv = vec![0u8; frame_size * 3 / 2];

    let scale_x = src_w as f32 / dst_w as f32;
    let scale_y = src_h as f32 / dst_h as f32;

    for dy in 0..dst_h {
        let sy = (dy as f32 * scale_y) as usize;
        for dx in 0..dst_w {
            let sx = (dx as f32 * scale_x) as usize;
            let i = sy * stride + sx * 4;
            let b = frame[i]     as i32;
            let g = frame[i + 1] as i32;
            let r = frame[i + 2] as i32;
            // Y
            let y = ((66 * r + 129 * g + 25 * b + 128) >> 8) + 16;
            yuv[dy * dst_w + dx] = y.clamp(0, 255) as u8;
        }
    }
    // U / V（4:2:0）
    let uv_start = frame_size;
    for dy in (0..dst_h).step_by(2) {
        let sy = (dy as f32 * scale_y) as usize;
        for dx in (0..dst_w).step_by(2) {
            let sx = (dx as f32 * scale_x) as usize;
            let i = sy * stride + sx * 4;
            let b = frame[i]     as i32;
            let g = frame[i + 1] as i32;
            let r = frame[i + 2] as i32;
            let u = ((-38 * r -  74 * g + 112 * b + 128) >> 8) + 128;
            let v = ((112 * r -  94 * g -  18 * b + 128) >> 8) + 128;
            let idx = (dy / 2) * (dst_w / 2) + dx / 2;
            yuv[uv_start + idx] = u.clamp(0, 255) as u8;
            yuv[uv_start + frame_size / 4 + idx] = v.clamp(0, 255) as u8;
        }
    }
    yuv
}

// ─── YUYV → YUV420 平面转换 ──────────────────────────────────────────────────

#[cfg(feature = "v4l2-capture")]
fn yuyv_to_yuv420(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    let frame_size = w * h;
    let mut yuv = vec![0u8; frame_size * 3 / 2];
    let y_plane  = &mut yuv[..frame_size];
    let uv_start = frame_size;

    for row in 0..h {
        for col in 0..w {
            let i = (row * w + col) * 2;
            y_plane[row * w + col] = src[i];
        }
    }
    for row in (0..h).step_by(2) {
        for col in (0..w).step_by(2) {
            let i = (row * w + col) * 2;
            let u = src[i + 1];
            let v = src[i + 3];
            let uv_idx = (row / 2) * (w / 2) + col / 2;
            yuv[uv_start + uv_idx] = u;
            yuv[uv_start + frame_size / 4 + uv_idx] = v;
        }
    }
    yuv
}


