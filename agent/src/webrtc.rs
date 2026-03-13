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
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};
use str0m::change::SdpAnswer;
use str0m::media::{Direction, Frequency, MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, Input, Output, Rtc};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::broadcast;
use tokio::net::UdpSocket;
use tracing::{info, warn};

pub async fn run(
    ice_cfg:              IceConfig,
    mut video_rx:         broadcast::Receiver<Bytes>,
    mut audio_rx:         broadcast::Receiver<Bytes>,
    hid_tx:               Sender<Bytes>,
    offer_tx:             tokio::sync::oneshot::Sender<String>,
    mut answer_rx:        Receiver<String>,
    mut remote_cand_rx:   Receiver<String>,
    _local_cand_tx:       Sender<String>,
) -> Result<()> {
    info!("webrtc: starting");

    // ─── 绑定 UDP socket（ICE 使用）─────────────────────────────────────────
    let socket = UdpSocket::bind("0.0.0.0:0").await.context("UDP bind")?;
    let socket_addr = socket.local_addr()?;
    let local_port = socket_addr.port();
    info!("webrtc: UDP socket bound on port {}", local_port);

    // ─── 构建 str0m Rtc 实例 ─────────────────────────────────────────────────
    let _ = &ice_cfg; // ice_cfg 字段预留
    let mut rtc = Rtc::builder().build();

    // 枚举本机真实网络接口 IP，注册为 ICE host candidate
    // 0.0.0.0 不是合法 candidate，需要逐个注册真实 IP
    let local_ips = get_local_ips();
    if local_ips.is_empty() {
        warn!("webrtc: no usable network interfaces found, using 127.0.0.1");
        let addr = SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), local_port);
        rtc.add_local_candidate(
            Candidate::host(addr, Protocol::Udp).context("host candidate")?,
        );
    } else {
        for ip in &local_ips {
            let addr = SocketAddr::new(*ip, local_port);
            info!("webrtc: registering local candidate {}", addr);
            rtc.add_local_candidate(
                Candidate::host(addr, Protocol::Udp).context("host candidate")?,
            );
        }
    }

    // ─── 生成 SDP offer（video + audio + DataChannel）────────────────────────
    let (offer_sdp, pending, video_mid, audio_mid) = build_full_offer(&mut rtc)?;
    info!("webrtc: SDP offer created ({} bytes)", offer_sdp.len());

    offer_tx.send(offer_sdp).map_err(|_| anyhow::anyhow!("offer channel closed"))?;

    // ─── 等待 SDP answer（无超时：等浏览器主动连接）──────────────────────────
    info!("webrtc: waiting for browser to connect…");
    let answer_sdp = answer_rx.recv()
        .await
        .context("answer channel closed")?;
    info!("webrtc: received SDP answer");

    let answer = SdpAnswer::from_sdp_string(&answer_sdp)?;
    rtc.sdp_api().accept_answer(pending, answer)?;

    // ─── 主事件循环 ───────────────────────────────────────────────────────────
    let mut video_ts:  u32 = 0;
    let mut audio_ts:  u32 = 0;
    let frame_dur_90k = 90000 / 15u32;
    let audio_dur_90k = 960u32; // Opus: 20ms @ 48kHz = 960 samples
    let mut buf = vec![0u8; 65536];

    loop {
        // 处理待设置的远端 ICE candidate
        while let Ok(cand) = remote_cand_rx.try_recv() {
            if let Ok(c) = Candidate::from_sdp_string(&cand) {
                rtc.add_remote_candidate(c);
            }
        }

        // 发送视频帧
        if let Ok(frame) = video_rx.try_recv() {
            send_video(&mut rtc, video_mid, &frame, video_ts);
            video_ts = video_ts.wrapping_add(frame_dur_90k);
        }

        // 发送音频帧
        if let Ok(frame) = audio_rx.try_recv() {
            send_audio(&mut rtc, audio_mid, &frame, audio_ts);
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
                handle_event(event, &hid_tx).await;
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
                    if let Ok(recv) = Receive::new(Protocol::Udp, from, socket_addr, &buf[..n]) {
                        rtc.handle_input(Input::Receive(Instant::now(), recv))?;
                    }
                }
            }
        }
    }
}

// ─── 辅助函数 ─────────────────────────────────────────────────────────────────

use str0m::change::SdpPendingOffer;

fn build_full_offer(rtc: &mut Rtc) -> Result<(String, SdpPendingOffer, Mid, Mid)> {
    let mut change = rtc.sdp_api();
    let video_mid = change.add_media(str0m::media::MediaKind::Video, Direction::SendOnly, None, None);
    let audio_mid = change.add_media(str0m::media::MediaKind::Audio, Direction::SendOnly, None, None);
    change.add_channel("hid-control".to_string());
    let (offer, pending) = change.apply()
        .ok_or_else(|| anyhow::anyhow!("SDP apply returned None — no media changes"))?;
    Ok((offer.to_sdp_string(), pending, video_mid, audio_mid))
}

fn send_video(rtc: &mut Rtc, mid: Mid, data: &[u8], ts: u32) {
    let Some(writer) = rtc.writer(mid) else { return };
    let pt = writer.payload_params().next().map(|p| p.pt());
    let Some(pt) = pt else { return };
    let _ = writer.write(pt, Instant::now(), MediaTime::from_90khz(ts as i64), data.to_vec());
}

fn send_audio(rtc: &mut Rtc, mid: Mid, data: &[u8], ts: u32) {
    let Some(writer) = rtc.writer(mid) else { return };
    let pt = writer.payload_params().next().map(|p| p.pt());
    let Some(pt) = pt else { return };
    let _ = writer.write(pt, Instant::now(), MediaTime::new(ts as i64, Frequency::FORTY_EIGHT_KHZ), data.to_vec());
}

async fn handle_event(event: Event, hid_tx: &Sender<Bytes>) {
    match event {
        Event::ChannelData(data) => {
            let _ = hid_tx.send(Bytes::copy_from_slice(&data.data)).await;
        }
        Event::IceConnectionStateChange(s) => {
            info!("ICE connection state: {:?}", s);
        }
        _ => {}
    }
}

/// 枚举本机可用的非回环、非 link-local 的网络接口 IP 地址
fn get_local_ips() -> Vec<IpAddr> {
    use std::net::{Ipv4Addr, UdpSocket as StdUdp};

    let mut ips = Vec::new();

    // 方法 1：通过连接外部地址让 OS 选择最佳出口 IP
    if let Ok(sock) = StdUdp::bind("0.0.0.0:0") {
        // connect 不会真的发包，只是让 OS 填充本地地址
        if sock.connect("8.8.8.8:80").is_ok() {
            if let Ok(addr) = sock.local_addr() {
                let ip = addr.ip();
                if !ip.is_loopback() && !ip.is_unspecified() {
                    ips.push(ip);
                }
            }
        }
    }

    // 方法 2：解析 /proc/net/fib_trie（Linux 特有）
    // 格式：先出现 "|-- <IP>"，下一行如果是 "/32 host LOCAL" 则该 IP 是本机地址
    if let Ok(content) = std::fs::read_to_string("/proc/net/fib_trie") {
        let mut last_ip: Option<Ipv4Addr> = None;
        for line in content.lines() {
            let trimmed = line.trim();
            if let Some(ip_str) = trimmed.strip_prefix("|-- ") {
                last_ip = ip_str.parse::<Ipv4Addr>().ok();
            } else if trimmed.starts_with("/32 host LOCAL") {
                if let Some(ip) = last_ip.take() {
                    let ip = IpAddr::V4(ip);
                    if !ip.is_loopback() && !ip.is_unspecified() && !ips.contains(&ip) {
                        ips.push(ip);
                    }
                }
            }
        }
    }

    ips
}
