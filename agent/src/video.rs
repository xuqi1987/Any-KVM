//! video.rs — V4L2 视频采集 + H.264 编码
//!
//! 流程：
//!   1. 打开 V4L2 设备
//!   2. 优先协商 V4L2_PIX_FMT_H264（硬件 M2M 编码器）
//!   3. 若不支持，回退到 YUYV + openh264 软编码
//!   4. 将 Annex-B NAL 帧发送到 tokio channel

use crate::config::VideoConfig;
use anyhow::{bail, Context, Result};
use bytes::Bytes;
use tokio::sync::mpsc::Sender;
use tracing::{info, warn};
use v4l::buffer::Type;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;
use v4l::{Device, FourCC};

pub fn run(cfg: VideoConfig, tx: Sender<Bytes>) -> Result<()> {
    info!(
        "video: opening {} ({}×{}@{}fps, target {}kbps)",
        cfg.device.display(), cfg.width, cfg.height, cfg.fps, cfg.bitrate_kbps
    );

    let dev = Device::with_path(&cfg.device)
        .with_context(|| format!("cannot open V4L2 device {:?}", cfg.device))?;

    // 尝试硬件 H.264 输出
    let hw_ok = cfg.hw_encode && try_hw_h264(&dev, &cfg, &tx).is_ok();

    if hw_ok {
        info!("video: using V4L2 M2M hardware H.264 encoder");
        // hw_h264 已在 try_hw_h264 循环中发送，不会到这里
    } else {
        warn!("video: hardware H.264 not available, falling back to software encoding (openh264)");
        run_sw_h264(&dev, &cfg, &tx)?;
    }

    Ok(())
}

// ─── 硬件 H.264（V4L2 直接输出 H.264，典型于 Amlogic / Rockchip）────────────

fn try_hw_h264(dev: &Device, cfg: &VideoConfig, tx: &Sender<Bytes>) -> Result<()> {
    let mut fmt = dev.format()?;
    fmt.width  = cfg.width;
    fmt.height = cfg.height;
    fmt.fourcc = FourCC::new(b"H264");
    let actual = dev.set_format(&fmt)?;

    if actual.fourcc != FourCC::new(b"H264") {
        bail!("device does not support H264 output format");
    }

    // 设置帧率
    let mut params = dev.params()?;
    params.interval = v4l::Fraction { numerator: 1, denominator: cfg.fps };
    dev.set_params(&params)?;

    info!("video: HW H264 {}×{} @{}fps", actual.width, actual.height, cfg.fps);

    let mut stream = v4l::io::mmap::Stream::with_buffers(dev, Type::VideoCapture, 4)
        .context("failed to create V4L2 mmap stream")?;

    loop {
        let (buf, _meta) = stream.next()?;
        let frame = Bytes::copy_from_slice(buf);
        if tx.blocking_send(frame).is_err() {
            break; // receiver 已关闭，退出循环
        }
    }
    Ok(())
}

// ─── 软件 H.264（YUYV → openh264 Baseline 编码）──────────────────────────────

fn run_sw_h264(dev: &Device, cfg: &VideoConfig, tx: &Sender<Bytes>) -> Result<()> {
    // 请求 YUYV 格式
    let mut fmt = dev.format()?;
    fmt.width  = cfg.width;
    fmt.height = cfg.height;
    fmt.fourcc = FourCC::new(b"YUYV");
    let actual = dev.set_format(&fmt)?;
    if actual.fourcc != FourCC::new(b"YUYV") {
        bail!("device does not support YUYV format for software encoding");
    }

    let mut params = dev.params()?;
    params.interval = v4l::Fraction { numerator: 1, denominator: cfg.fps };
    dev.set_params(&params)?;

    info!("video: SW H264 {}×{} @{}fps via openh264", cfg.width, cfg.height, cfg.fps);

    // 初始化 openh264 编码器
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

        // YUYV → YUV420 planar（openh264 输入格式）
        let yuv = yuyv_to_yuv420(buf, w, h);
        let src = openh264::formats::YUVBuffer::from_vec(yuv, w, h);

        let bitstream = encoder.encode(&src).context("openh264 encode error")?;
        let encoded = Bytes::from(bitstream.to_vec());
        if !encoded.is_empty() && tx.blocking_send(encoded).is_err() {
            return Ok(());
        }
    }
}

// ─── YUYV → YUV420 平面转换 ──────────────────────────────────────────────────

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
    // U/V 4:2:0 下采样
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


