mod config;
mod video;
mod audio;
mod hid;
mod webrtc;
mod signal_client;

use anyhow::Result;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
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
    let keyframe_flag = Arc::new(AtomicBool::new(false));

    // 视频控制通道（分辨率/帧率运行时切换）
    let (video_ctrl_tx, video_ctrl_rx) = std::sync::mpsc::channel::<hid::VideoControl>();

    let video_cfg = cfg.video.clone();
    let kf_flag = keyframe_flag.clone();
    tokio::task::spawn_blocking(move || {
        match video::run_with_ctrl(video_cfg.clone(), video_tx.clone(), kf_flag.clone(), Some(video_ctrl_rx)) {
            Ok(()) => {}
            Err(e) => {
                error!("video module error: {:#}", e);
                // 自动回退到屏幕截图
                if video_cfg.source != "screen" {
                    warn!("video: falling back to screen capture");
                    let mut screen_cfg = video_cfg;
                    screen_cfg.source = "screen".to_string();
                    if let Err(e2) = video::run(screen_cfg, video_tx, kf_flag) {
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
        if let Err(e) = hid::run(hid_cfg, hid_rx, Some(video_ctrl_tx)) {
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

    // 将 mpsc receiver 桥接到 broadcast（peer 连接时才消费，否则背压到 video 模块）
    let peer_connected = Arc::new(AtomicBool::new(false));
    {
        let mut vr = video_rx;
        let vbt = video_bcast_tx.clone();
        let pc = peer_connected.clone();
        tokio::spawn(async move {
            let mut was_idle = true;
            loop {
                if !pc.load(std::sync::atomic::Ordering::Relaxed) {
                    // 无浏览器连接：不消费，让 mpsc channel 填满触发 video 模块背压
                    was_idle = true;
                    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                    continue;
                }
                // 刚从 idle 恢复：drain 旧帧避免发送过时数据
                if was_idle {
                    was_idle = false;
                    while vr.try_recv().is_ok() {}
                }
                match vr.recv().await {
                    Some(frame) => { let _ = vbt.send(frame); }
                    None => break,
                }
            }
        });
    }
    {
        let mut ar = audio_rx;
        let abt = audio_bcast_tx.clone();
        let pc = peer_connected.clone();
        tokio::spawn(async move {
            let mut was_idle = true;
            loop {
                if !pc.load(std::sync::atomic::Ordering::Relaxed) {
                    was_idle = true;
                    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                    continue;
                }
                if was_idle {
                    was_idle = false;
                    while ar.try_recv().is_ok() {}
                }
                match ar.recv().await {
                    Some(frame) => { let _ = abt.send(frame); }
                    None => break,
                }
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
        let video_fps = cfg.video.fps;
        let kf = keyframe_flag.clone();
        let pc = peer_connected.clone();
        let webrtc_handle = tokio::spawn(async move {
            if let Err(e) = webrtc::run(
                ice_cfg,
                video_fps,
                video_rx_this,
                audio_rx_this,
                hid_tx_this,
                offer_tx,
                answer_rx,
                remote_cand_rx,
                local_cand_tx,
                kf,
                pc,
            ).await {
                error!("webrtc module error: {:#}", e);
            }
        });
        let webrtc_abort = webrtc_handle.abort_handle();

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
        let signal_abort = signal_handle.abort_handle();

        info!("reconnect: starting new session");
        let session_start = std::time::Instant::now();

        tokio::select! {
            _ = webrtc_handle  => {
                signal_abort.abort();
                peer_connected.store(false, std::sync::atomic::Ordering::Relaxed);
                warn!("webrtc session ended, reconnecting in {:?}…", reconnect_delay);
            }
            _ = signal_handle  => {
                webrtc_abort.abort();
                peer_connected.store(false, std::sync::atomic::Ordering::Relaxed);
                warn!("signal session ended, reconnecting in {:?}…", reconnect_delay);
            }
            _ = tokio::signal::ctrl_c() => {
                webrtc_abort.abort();
                signal_abort.abort();
                info!("Ctrl-C received, shutting down");
                return Ok(());
            }
        }

        // 若会话持续超过 10s，视为成功，重置退避延迟
        if session_start.elapsed() > Duration::from_secs(10) {
            reconnect_delay = Duration::from_secs(3);
        } else {
            // 指数退避：3s → 5s → 8s → … 最多 30s
            reconnect_delay = (reconnect_delay * 3 / 2).min(Duration::from_secs(30));
        }

        tokio::time::sleep(reconnect_delay).await;
    }
}
