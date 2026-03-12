mod config;
mod video;
mod audio;
mod hid;
mod webrtc;
mod signal_client;

use anyhow::Result;
use std::sync::Arc;
use tracing::{info, error};

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

    // ─── 启动各功能模块 ───────────────────────────────────────────────────────
    // video 模块输出 H.264 Annex-B NAL 帧（通过 tokio channel）
    let (video_tx, video_rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(32);
    let video_cfg = cfg.video.clone();
    tokio::task::spawn_blocking(move || {
        if let Err(e) = video::run(video_cfg, video_tx) {
            error!("video module error: {:#}", e);
        }
    });

    // audio 模块输出 Opus 帧（通过 tokio channel）
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

    // WebRTC 引擎（发送媒体、接收 DataChannel HID 控制）
    let (offer_tx, offer_rx) = tokio::sync::oneshot::channel::<String>();
    let (answer_tx, answer_rx) = tokio::sync::mpsc::channel::<String>(4);
    let (remote_candidate_tx, remote_candidate_rx) =
        tokio::sync::mpsc::channel::<String>(64);
    let (local_candidate_tx, mut local_candidate_rx) =
        tokio::sync::mpsc::channel::<String>(64);

    let ice_cfg = cfg.ice.clone();
    let webrtc_handle = tokio::spawn(async move {
        if let Err(e) = webrtc::run(
            ice_cfg,
            video_rx,
            audio_rx,
            hid_tx,
            offer_tx,
            answer_rx,
            remote_candidate_rx,
            local_candidate_tx,
        ).await {
            error!("webrtc module error: {:#}", e);
        }
    });

    // 信令客户端（连接信令服务器，交换 SDP+ICE）
    let sig_cfg = cfg.signal.clone();
    let signal_handle = tokio::spawn(async move {
        if let Err(e) = signal_client::run(
            sig_cfg,
            offer_rx,
            answer_tx,
            remote_candidate_tx,
            &mut local_candidate_rx,
        ).await {
            error!("signal_client module error: {:#}", e);
        }
    });

    // 等待主任务结束（正常情况下永久运行）
    tokio::select! {
        _ = webrtc_handle  => { error!("WebRTC task exited"); }
        _ = signal_handle  => { error!("Signal task exited"); }
        _ = tokio::signal::ctrl_c() => {
            info!("Ctrl-C received, shutting down");
        }
    }

    Ok(())
}
