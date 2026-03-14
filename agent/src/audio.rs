//! audio.rs — ALSA PCM 采集 + Opus 编码
//!
//! 流程：
//!   ALSA PCM → S16_LE 帧 → audiopus Opus 编码 → Opus 包 → tokio channel

use crate::config::AudioConfig;
use anyhow::{Context, Result};
use audiopus::{coder::Encoder, Application, Channels, SampleRate};
use bytes::Bytes;
use tokio::sync::mpsc::Sender;
use tracing::info;

/// 每次编码的样本数（20ms，Opus 推荐帧大小）
const FRAME_SAMPLES: usize = 960; // 48000 Hz × 20ms = 960

pub fn run(cfg: AudioConfig, tx: Sender<Bytes>) -> Result<()> {
    info!(
        "audio: opening ALSA device '{}' ({}Hz, {}ch, {}kbps Opus)",
        cfg.device, cfg.sample_rate, cfg.channels, cfg.bitrate_kbps
    );

    let alsa = alsa::PCM::new(&cfg.device, alsa::Direction::Capture, false)
        .with_context(|| format!("cannot open ALSA device '{}'", cfg.device))?;

    // ─── 配置 ALSA PCM 参数 ─────────────────────────────────────────────────
    {
        let hwp = alsa::pcm::HwParams::any(&alsa).context("ALSA hwparams")?;
        hwp.set_channels(cfg.channels).context("ALSA set channels")?;
        hwp.set_rate(cfg.sample_rate, alsa::ValueOr::Nearest)?;
        hwp.set_format(alsa::pcm::Format::s16()).context("ALSA set format")?;
        hwp.set_access(alsa::pcm::Access::RWInterleaved)?;
        // 缓冲区大小设为 4 帧，降低延迟
        hwp.set_buffer_size((FRAME_SAMPLES * 4) as _)?;
        hwp.set_period_size(FRAME_SAMPLES as _, alsa::ValueOr::Nearest)?;
        alsa.hw_params(&hwp)?;
    }
    alsa.prepare()?;

    // ─── 初始化 Opus 编码器 ──────────────────────────────────────────────────
    let channels = if cfg.channels == 1 { Channels::Mono } else { Channels::Stereo };
    let mut encoder = Encoder::new(SampleRate::Hz48000, channels, Application::Audio)
        .context("failed to create Opus encoder")?;
    encoder.set_bitrate(audiopus::Bitrate::BitsPerSecond(
        (cfg.bitrate_kbps * 1000) as i32,
    ))?;

    info!("audio: ALSA + Opus encoder ready");

    let io = alsa.io_i16().context("ALSA i16 io")?;
    let mut pcm_buf  = vec![0i16; FRAME_SAMPLES * cfg.channels as usize];
    let mut opus_buf = vec![0u8; 4096];

    loop {
        // 读取一帧 PCM
        let read = io.readi(&mut pcm_buf).unwrap_or_else(|e| {
            // 处理设备被拔出等情况
            tracing::warn!("ALSA read error: {}, attempting recovery", e);
            alsa.recover(e.errno() as i32, false).ok();
            0
        });
        if read == 0 { continue; }

        // Opus 编码（输入 i16 切片，输出 opus_buf）
        let len = encoder
            .encode(&pcm_buf[..read * cfg.channels as usize], &mut opus_buf)
            .context("Opus encode error")?;

        let frame = Bytes::copy_from_slice(&opus_buf[..len]);
        if tx.blocking_send(frame).is_err() {
            break; // receiver 关闭，正常退出
        }
    }

    Ok(())
}
