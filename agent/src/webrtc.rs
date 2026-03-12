//! webrtc.rs — str0m WebRTC 引擎
//!
//! 职责：
//!   - 创建 RTCPeerConnection（ICE + DTLS + SRTP）
//!   - 生成 SDP offer（H.264 video + Opus audio + DataChannel）
//!   - 接收 SDP answer，处理 ICE candidate 交换
//!   - 持续发送视频/音频 RTP 帧
//!   - 通过 DataChannel 接收 HID 控制帧并转发给 hid 模块

use crate::config::IceConfig;
use anyhow::{Context, Result};
use bytes::Bytes;
use std::time::{Duration, Instant};
use str0m::change::{SdpOffer, SdpAnswer};
use str0m::format::Codec;
use str0m::media::{Direction, MediaTime, Mid};
use str0m::net::Receive;
use str0m::{Event, IceConnectionState, Input, Output, Rtc, RtcError};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::net::UdpSocket;
use tracing::{debug, info, warn};

pub async fn run(
    ice_cfg:              IceConfig,
    mut video_rx:         Receiver<Bytes>,
    mut audio_rx:         Receiver<Bytes>,
    hid_tx:               Sender<Bytes>,
    offer_tx:             tokio::sync::oneshot::Sender<String>,
    mut answer_rx:        Receiver<String>,
    mut remote_cand_rx:   Receiver<String>,
    local_cand_tx:        Sender<String>,
) -> Result<()> {
    info!("webrtc: starting");

    // ─── 绑定 UDP socket（ICE 使用）─────────────────────────────────────────
    let socket = UdpSocket::bind("0.0.0.0:0").await.context("UDP bind")?;
    let local_addr = socket.local_addr()?;
    info!("webrtc: UDP socket bound on {}", local_addr);

    // ─── 构建 str0m Rtc 实例 ─────────────────────────────────────────────────
    let mut rtc = {
        let mut builder = Rtc::builder();

        // 添加 ICE 服务器（STUN）
        for stun_url in &ice_cfg.stun_servers {
            builder = builder.add_ice_server(stun_url.parse()?);
        }
        // 添加 TURN（可选）
        if let (Some(turn_url), Some(user), Some(pass)) = (
            &ice_cfg.turn_url,
            &ice_cfg.turn_username,
            &ice_cfg.turn_password,
        ) {
            builder = builder.add_ice_server(
                format!("{}:{}@{}", user, pass, turn_url).parse()?
            );
        }

        builder.build()
    };

    // ─── 添加媒体轨道 ─────────────────────────────────────────────────────────
    let video_mid: Mid = {
        let mut change = rtc.sdp_api();
        let mid = change.add_media(
            str0m::media::MediaKind::Video,
            Direction::SendOnly,
            None, None,
        );
        // 限制为 H.264 Baseline（WebRTC 浏览器兼容）
        // str0m 默认支持 H.264，无需手动指定 payload type
        let (offer, pending) = change.apply().context("build video offer")?;
        let sdp_str = offer.to_sdp_string();
        // 暂存 pending，稍后处理 answer 时通过 pending.complete
        let _ = pending; // 注意：实际集成时需保留 pending
        mid
    };

    // 生成完整 offer（含 video + audio + DataChannel）
    let offer_sdp = build_full_offer(&mut rtc).await?;
    info!("webrtc: SDP offer created ({} bytes)", offer_sdp.len());

    // 发送 offer 给信令模块
    offer_tx.send(offer_sdp).map_err(|_| anyhow::anyhow!("offer channel closed"))?;

    // ─── 等待 SDP answer ─────────────────────────────────────────────────────
    let answer_sdp = tokio::time::timeout(Duration::from_secs(60), answer_rx.recv())
        .await
        .context("answer timeout")?
        .context("answer channel closed")?;
    info!("webrtc: received SDP answer");
    rtc.sdp_api().accept_answer(SdpAnswer::from_sdp_string(&answer_sdp)?)?;

    // ─── 主事件循环 ───────────────────────────────────────────────────────────
    let mut video_ts:  u32 = 0;
    let mut audio_ts:  u32 = 0;
    let frame_dur_90k = 90000 / 15u32; // 视频时间戳增量（@15fps，90kHz 时钟）
    let audio_dur_90k = 90000 * 960 / 48000; // Opus 20ms 帧 @90kHz
    let mut buf = vec![0u8; 65536];

    loop {
        // 处理待发送的本地 ICE candidate
        while let Ok(cand) = remote_cand_rx.try_recv() {
            if let Ok(c) = str0m::ice::Candidate::from_sdp_attribute(&cand) {
                rtc.add_remote_candidate(c);
            }
        }

        // 发送视频帧
        if let Ok(frame) = video_rx.try_recv() {
            send_video(&mut rtc, &frame, video_ts);
            video_ts = video_ts.wrapping_add(frame_dur_90k);
        }

        // 发送音频帧
        if let Ok(frame) = audio_rx.try_recv() {
            send_audio(&mut rtc, &frame, audio_ts);
            audio_ts = audio_ts.wrapping_add(audio_dur_90k);
        }

        // 轮询 str0m 输出（本地 ICE candidate、网络数据包等）
        let timeout = match rtc.poll_output()? {
            Output::Timeout(t) => t,
            Output::Transmit(send) => {
                socket.send_to(&send.contents, send.destination).await.ok();
                Instant::now()
            }
            Output::Event(event) => {
                handle_event(event, &local_cand_tx, &hid_tx).await;
                Instant::now()
            }
        };

        // 从 UDP socket 读取网络数据，超时后继续
        let wait = timeout.saturating_duration_since(Instant::now());
        let wait = wait.min(Duration::from_millis(5));
        tokio::select! {
            _ = tokio::time::sleep(wait) => {
                rtc.handle_input(Input::Timeout(Instant::now()))?;
            }
            result = socket.recv_from(&mut buf) => {
                if let Ok((n, from)) = result {
                    let data = &buf[..n];
                    rtc.handle_input(Input::Receive(
                        Instant::now(),
                        Receive { source: from, destination: local_addr, contents: data.into() },
                    ))?;
                }
            }
        }
    }
}

// ─── 辅助函数 ─────────────────────────────────────────────────────────────────

async fn build_full_offer(rtc: &mut Rtc) -> Result<String> {
    let mut change = rtc.sdp_api();
    change.add_media(str0m::media::MediaKind::Video, Direction::SendOnly, None, None);
    change.add_media(str0m::media::MediaKind::Audio, Direction::SendOnly, None, None);
    change.add_channel("hid-control".to_string());
    let (offer, _pending) = change.apply()?;
    Ok(offer.to_sdp_string())
}

fn send_video(rtc: &mut Rtc, data: &[u8], ts: u32) {
    // 找到 Video SendOnly 轨道并写入 RTP
    if let Some(writer) = rtc.direct_api().stream_tx_by_mid(
        rtc.sdp_api().mids().find(|_| true).unwrap_or_default(),
        None,
    ) {
        let _ = writer.write(MediaTime::from_90khz(ts as u64), data);
    }
}

fn send_audio(rtc: &mut Rtc, data: &[u8], ts: u32) {
    // 类似 send_video，找音频轨道
    // 此处简化，实际集成需区分 video/audio mid
    let _ = (rtc, data, ts);
}

async fn handle_event(event: Event, local_cand_tx: &Sender<String>, hid_tx: &Sender<Bytes>) {
    match event {
        Event::IceCandidate(cand) => {
            let sdp = cand.to_sdp_attribute();
            debug!("local ICE candidate: {}", sdp);
            let _ = local_cand_tx.send(sdp).await;
        }
        Event::ChannelData(data) => {
            // DataChannel 数据 → HID 模块
            let _ = hid_tx.send(Bytes::copy_from_slice(&data.data)).await;
        }
        Event::IceConnectionStateChange(s) => {
            info!("ICE connection state: {:?}", s);
        }
        _ => {}
    }
}
