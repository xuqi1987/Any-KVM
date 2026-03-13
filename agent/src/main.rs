mod config;
mod video;
mod audio;
mod hid;
mod webrtc;
mod signal_client;

use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn, error};

#[tokio::main]
async fn main() -> Result<()> {
    // ─── 日志初始化 ──────────────────────────────────────────────────────────
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("any_kvm_agent=debug".parse()?)
        )
        .init();

    info!("Any-KVM Agent starting…");

    // ─── 读取配置文件 ─────────────────────────────────────────────────────────
    let config_path = std::env::args().nth(1)
        .unwrap_or_else(|| "config.toml".to_string());
    let cfg = Arc::new(config::Config::load(&config_path)?);
    info!("config loaded from '{}'", config_path);
    info!("signal server: {}  room: {}", cfg.signal.url, cfg.signal.room_id);

    // ─── 启动媒体采集模块（只启动一次，全程运行）───────────────────────────────
    // video 模块输出 H.264 Annex-B NAL 帧
    // 如果配置的源（默认 v4l2）失败，自动回退到屏幕截图
    let (video_tx, video_rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(32);
    let video_cfg = cfg.video.clone();
    tokio::task::spawn_blocking(move || {
        match video::run(video_cfg.clone(), video_tx.clone()) {
            Ok(()) => {}
            Err(e) => {
                error!("video module error: {:#}", e);
                // 自动回退到屏幕截图
                if video_cfg.source != "screen" {
                    warn!("video: falling back to screen capture");
                    let mut screen_cfg = video_cfg;
                    screen_cfg.source = "screen".to_string();
                    if let Err(e2) = video::run(screen_cfg, video_tx) {
                        error!("screen capture fallback also failed: {:#}", e2);
                    }
                }
            }
        }
    });

    // audio 模块输出 Opus 帧
    let (audio_tx, audio_rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(64);
    if cfg.audio.enabled {
        let audio_cfg = cfg.audio.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(e) = audio::run(audio_cfg, audio_tx) {
                error!("audio module error: {:#}", e);
            }
        });
    }

    // hid 模块：从 DataChannel 收取控制帧
    let (hid_tx, hid_rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(64);
    let hid_cfg = cfg.hid.clone();
    tokio::task::spawn_blocking(move || {
        if let Err(e) = hid::run(hid_cfg, hid_rx) {
            error!("hid module error: {:#}", e);
        }
    });

    // ─── 信令 + WebRTC 循环（浏览器断开后自动重新等待下一个连接）─────────────────
    // 使用 Arc 共享 receiver，重建 sender 实现「多路广播」
    // 简化方案：持有 Arc<Mutex<Receiver>> 会增加复杂度，这里直接重新创建 channel
    // video/audio channel 只有 sender 端一直存活，receiver 在每轮 WebRTC 中消耗。
    // 为避免 receiver 被 move 而无法复用，改用 broadcast channel:
    use tokio::sync::broadcast;
    let (video_bcast_tx, _) = broadcast::channel::<bytes::Bytes>(32);
    let (audio_bcast_tx, _) = broadcast::channel::<bytes::Bytes>(64);

    // 将 mpsc receiver 桥接到 broadcast
    {
        let mut vr = video_rx;
        let vbt = video_bcast_tx.clone();
        tokio::spawn(async move {
            while let Some(frame) = vr.recv().await {
                let _ = vbt.send(frame);
            }
        });
    }
    {
        let mut ar = audio_rx;
        let abt = audio_bcast_tx.clone();
        tokio::spawn(async move {
            while let Some(frame) = ar.recv().await {
                let _ = abt.send(frame);
            }
        });
    }

    let mut reconnect_delay = Duration::from_secs(3);

    loop {
        // 每轮为 WebRTC/信令创建新 channel
        let (offer_tx, offer_rx)  = tokio::sync::oneshot::channel::<String>();
        let (answer_tx, answer_rx) = tokio::sync::mpsc::channel::<String>(4);
        let (remote_cand_tx, remote_cand_rx) = tokio::sync::mpsc::channel::<String>(64);
        let (local_cand_tx, mut local_cand_rx) = tokio::sync::mpsc::channel::<String>(64);

        // 为本轮 WebRTC 订阅视频/音频 broadcast
        let video_rx_this = video_bcast_tx.subscribe();
        let audio_rx_this = audio_bcast_tx.subscribe();

        // 克隆 hid_tx，允许多轮 WebRTC 共享同一个 hid 模块
        let hid_tx_this = hid_tx.clone();

        let ice_cfg = cfg.ice.clone();
        let webrtc_handle = tokio::spawn(async move {
            if let Err(e) = webrtc::run(
                ice_cfg,
                video_rx_this,
                audio_rx_this,
                hid_tx_this,
                offer_tx,
                answer_rx,
                remote_cand_rx,
                local_cand_tx,
            ).await {
                error!("webrtc module error: {:#}", e);
            }
        });

        let sig_cfg = cfg.signal.clone();
        let signal_handle = tokio::spawn(async move {
            if let Err(e) = signal_client::run(
                sig_cfg,
                offer_rx,
                answer_tx,
                remote_cand_tx,
                &mut local_cand_rx,
            ).await {
                error!("signal_client module error: {:#}", e);
            }
        });

        tokio::select! {
            _ = webrtc_handle  => { warn!("webrtc session ended, reconnecting in {:?}…", reconnect_delay); }
            _ = signal_handle  => { warn!("signal session ended, reconnecting in {:?}…", reconnect_delay); }
            _ = tokio::signal::ctrl_c() => {
                info!("Ctrl-C received, shutting down");
                return Ok(());
            }
        }

        tokio::time::sleep(reconnect_delay).await;
        // 退避：首次 3s，逐步增加到最多 30s
        reconnect_delay = (reconnect_delay + Duration::from_secs(2)).min(Duration::from_secs(30));
        info!("reconnect: starting new session");
    }
}
