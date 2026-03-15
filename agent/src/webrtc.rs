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
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use str0m::change::SdpAnswer;
use str0m::media::{Direction, Frequency, MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, Input, Output, Rtc};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::broadcast;
use tokio::net::UdpSocket;
use tracing::{info, warn, debug};

pub async fn run(
    ice_cfg:              IceConfig,
    video_fps:            u32,
    mut video_rx:         broadcast::Receiver<Bytes>,
    mut audio_rx:         broadcast::Receiver<Bytes>,
    hid_tx:               Sender<Bytes>,
    offer_tx:             tokio::sync::oneshot::Sender<String>,
    mut answer_rx:        Receiver<String>,
    mut remote_cand_rx:   Receiver<String>,
    _local_cand_tx:       Sender<String>,
    keyframe_flag:        Arc<AtomicBool>,
    peer_connected:       Arc<AtomicBool>,
) -> Result<()> {
    info!("webrtc: starting");

    // ─── 绑定 UDP socket（ICE 使用）─────────────────────────────────────────
    let socket = UdpSocket::bind("0.0.0.0:0").await.context("UDP bind")?;
    let local_port = socket.local_addr()?.port();
    info!("webrtc: UDP socket bound on port {}", local_port);

    // ─── 确定本机所有 IP（用于 Receive::new 的 dst 地址匹配）─────────────────
    let local_ips = get_local_ips();
    let primary_ip = local_ips.first().copied()
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
    let local_addr = SocketAddr::new(primary_ip, local_port);
    // Build a list of all local socket addresses for Receive::new dst matching
    let local_addrs: Vec<SocketAddr> = if local_ips.is_empty() {
        vec![local_addr]
    } else {
        local_ips.iter().map(|&ip| SocketAddr::new(ip, local_port)).collect()
    };
    info!("webrtc: primary local address {}, all addrs: {:?}", local_addr, local_addrs);

    // ─── 构建 str0m Rtc 实例 ─────────────────────────────────────────────────
    let mut rtc = Rtc::builder().build();

    // 注册 host candidate（本机真实 IP）
    if local_ips.is_empty() {
        warn!("webrtc: no usable network interfaces found, using 127.0.0.1");
        rtc.add_local_candidate(
            Candidate::host(local_addr, Protocol::Udp).context("host candidate")?,
        );
    } else {
        for ip in &local_ips {
            let addr = SocketAddr::new(*ip, local_port);
            info!("webrtc: registering host candidate {}", addr);
            rtc.add_local_candidate(
                Candidate::host(addr, Protocol::Udp).context("host candidate")?,
            );
        }
    }

    // ─── STUN binding：发现公网 IP，注册 srflx candidate ─────────────────────
    for stun_url in &ice_cfg.stun_servers {
        match stun_binding(&socket, stun_url).await {
            Ok(srflx_addr) => {
                info!("webrtc: STUN srflx candidate {} (via {})", srflx_addr, stun_url);
                let cand = Candidate::server_reflexive(srflx_addr, local_addr, Protocol::Udp)
                    .context("srflx candidate")?;
                rtc.add_local_candidate(cand);
                break; // 一个成功即可
            }
            Err(e) => {
                warn!("webrtc: STUN binding to {} failed: {:#}", stun_url, e);
            }
        }
    }

    // ─── TURN allocation：获取 relay candidate（跨网必需）─────────────────────
    let mut turn_state: Option<TurnState> = None;
    let mut relay_addr: Option<SocketAddr> = None;

    if let (Some(turn_url), Some(user), Some(pass)) = (
        ice_cfg.turn_url.as_deref(),
        ice_cfg.turn_username.as_deref(),
        ice_cfg.turn_password.as_deref(),
    ) {
        match turn_allocate(&socket, turn_url, user, pass).await {
            Ok((raddr, state)) => {
                info!("webrtc: TURN relay candidate {} (via {})", raddr, turn_url);
                let cand = Candidate::relayed(raddr, Protocol::Udp)
                    .context("relay candidate")?;
                rtc.add_local_candidate(cand);
                relay_addr = Some(raddr);
                turn_state = Some(state);
            }
            Err(e) => {
                warn!("webrtc: TURN allocate failed: {:#}", e);
            }
        }
    }

    // ─── 生成 SDP offer（video + audio + DataChannel）────────────────────────
    let (offer_sdp, pending, video_mid, audio_mid) = build_full_offer(&mut rtc)?;
    info!("webrtc: SDP offer created ({} bytes)", offer_sdp.len());

    debug!("webrtc: SDP offer:\n{}", offer_sdp);
    offer_tx.send(offer_sdp).map_err(|_| anyhow::anyhow!("offer channel closed"))?;

    // ─── 等待 SDP answer（无超时：等浏览器主动连接）──────────────────────────
    info!("webrtc: waiting for browser to connect…");
    let answer_sdp = answer_rx.recv()
        .await
        .context("answer channel closed")?;
    info!("webrtc: received SDP answer ({} bytes)", answer_sdp.len());
    debug!("webrtc: SDP answer:\n{}", answer_sdp);

    let answer = SdpAnswer::from_sdp_string(&answer_sdp)?;
    rtc.sdp_api().accept_answer(pending, answer)?;

    // ─── 主事件循环 ───────────────────────────────────────────────────────────
    let mut ice_connected = false;
    let mut turn_perms: HashSet<IpAddr> = HashSet::new(); // 已创建 permission 的 peer IP
    let mut turn_refresh_at = Instant::now() + Duration::from_secs(240); // TURN refresh
    let mut video_ts:  u32 = 0;
    let mut audio_ts:  u32 = 0;
    let fps = if video_fps > 0 { video_fps } else { 15 };
    let frame_dur_90k = 90000 / fps;
    let audio_dur_90k = 960u32; // Opus: 20ms @ 48kHz = 960 samples
    let mut buf = vec![0u8; 65536];
    let mut video_frame_count: u64 = 0;
    let mut video_write_count: u64 = 0;
    let mut video_drop_count: u64 = 0;
    let mut transmit_count: u64 = 0;

    loop {
        // str0m 会在内部超时后将 alive 设为 false
        if !rtc.is_alive() {
            info!("webrtc: Rtc is no longer alive, exiting for reconnection");
            return Ok(());
        }

        // 处理待设置的远端 ICE candidate
        while let Ok(cand) = remote_cand_rx.try_recv() {
            if let Ok(c) = Candidate::from_sdp_string(&cand) {
                // 为远端 candidate 的 IP 创建 TURN permission
                if let Some(ref ts) = turn_state {
                    let peer_ip = c.addr().ip();
                    if !turn_perms.contains(&peer_ip) {
                        if let Err(e) = turn_create_permission(&socket, ts, peer_ip).await {
                            warn!("TURN CreatePermission for {} failed: {:#}", peer_ip, e);
                        } else {
                            debug!("TURN permission created for {}", peer_ip);
                            turn_perms.insert(peer_ip);
                        }
                    }
                }
                rtc.add_remote_candidate(c);
            }
        }

        // TURN Refresh (keep allocation alive)
        if turn_state.is_some() && Instant::now() >= turn_refresh_at {
            if let Some(ref ts) = turn_state {
                if let Err(e) = turn_refresh(&socket, ts).await {
                    warn!("TURN refresh failed: {:#}", e);
                }
            }
            turn_refresh_at = Instant::now() + Duration::from_secs(240);
        }

        // 仅在 ICE 连接后发送媒体（str0m 在 ICE 未连接时会静默丢弃写入的帧）
        if ice_connected {
            // 发送视频帧
            match video_rx.try_recv() {
                Ok(frame) => {
                    video_frame_count += 1;
                    if video_frame_count == 1 {
                        debug!("webrtc: first video frame ({} bytes), NAL types: {}",
                            frame.len(), describe_h264_nals(&frame));
                    }
                    send_video(&mut rtc, video_mid, &frame, video_ts, &mut video_write_count, &mut video_drop_count);
                    video_ts = video_ts.wrapping_add(frame_dur_90k);
                    if video_frame_count % 300 == 0 {
                        info!("webrtc: video stats — received={}, written={}, dropped={}, udp_tx={}",
                            video_frame_count, video_write_count, video_drop_count, transmit_count);
                    }
                }
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    warn!("webrtc: video broadcast lagged, skipped {} frames", n);
                }
                _ => {}
            }

            // 发送音频帧
            match audio_rx.try_recv() {
                Ok(frame) => {
                    send_audio(&mut rtc, audio_mid, &frame, audio_ts);
                    audio_ts = audio_ts.wrapping_add(audio_dur_90k);
                }
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    warn!("webrtc: audio broadcast lagged, skipped {} frames", n);
                }
                _ => {}
            }
        } else {
            // ICE 未连接时丢弃积压的帧，避免 broadcast lag
            while video_rx.try_recv().is_ok() {}
            while audio_rx.try_recv().is_ok() {}
        }

        // 轮询 str0m 输出 — 循环 poll 直到 Timeout，一次性发完所有排队的包
        let timeout;
        loop {
            match rtc.poll_output()? {
                Output::Timeout(t) => {
                    timeout = t;
                    break;
                }
                Output::Transmit(send) => {
                    transmit_count += 1;
                    // 如果源地址是 relay 地址，通过 TURN Send indication 发送
                    if let (Some(raddr), Some(ref ts)) = (relay_addr, &turn_state) {
                        if send.source == raddr {
                            let wrapped = build_turn_send_indication(send.destination, &send.contents);
                            socket.send_to(&wrapped, ts.turn_addr).await.ok();
                        } else {
                            socket.send_to(&send.contents, send.destination).await.ok();
                        }
                    } else {
                        socket.send_to(&send.contents, send.destination).await.ok();
                    }
                }
                Output::Event(event) => {
                    if handle_event(event, &hid_tx, &mut ice_connected, &keyframe_flag, &peer_connected).await {
                        info!("webrtc: session ended, returning for reconnection");
                        peer_connected.store(false, Ordering::Relaxed);
                        return Ok(());
                    }
                }
            }
        }

        // 从 UDP socket 读取网络数据，超时后继续
        // 使用 primary local IP 作为 dst，使 str0m 能匹配到已注册的候选地址
        let wait = timeout.saturating_duration_since(Instant::now());
        let wait = wait.min(Duration::from_millis(5));
        tokio::select! {
            _ = tokio::time::sleep(wait) => {
                rtc.handle_input(Input::Timeout(Instant::now()))?;
            }
            result = socket.recv_from(&mut buf) => {
                if let Ok((n, from)) = result {
                    // 检查是否来自 TURN server 的 Data indication 或 ChannelData
                    if let Some(ref ts) = turn_state {
                        if from == ts.turn_addr {
                            if let Some(raddr) = relay_addr {
                                if let Some((peer_addr, data)) = parse_turn_data(&buf[..n]) {
                                    // TURN 中继数据：peer → TURN → 我们
                                    if let Ok(recv) = Receive::new(Protocol::Udp, peer_addr, raddr, data) {
                                        rtc.handle_input(Input::Receive(Instant::now(), recv))?;
                                    }
                                } else {
                                    // 可能是 TURN 控制消息（Allocate/Refresh 响应等），忽略
                                }
                            }
                            continue;
                        }
                    }
                    // 普通 UDP 数据：尝试所有本地候选地址匹配 dst，找到第一个成功的
                    for &la in &local_addrs {
                        match Receive::new(Protocol::Udp, from, la, &buf[..n]) {
                            Ok(recv) => {
                                rtc.handle_input(Input::Receive(Instant::now(), recv))?;
                                break;
                            }
                            Err(_) => {}
                        }
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
    let video_mid = change.add_media(str0m::media::MediaKind::Video, Direction::SendOnly, Some("anykvm".into()), None);
    let audio_mid = change.add_media(str0m::media::MediaKind::Audio, Direction::SendOnly, Some("anykvm".into()), None);
    change.add_channel("hid-control".to_string());
    let (offer, pending) = change.apply()
        .ok_or_else(|| anyhow::anyhow!("SDP apply returned None — no media changes"))?;
    Ok((offer.to_sdp_string(), pending, video_mid, audio_mid))
}

fn send_video(rtc: &mut Rtc, mid: Mid, data: &[u8], ts: u32, write_count: &mut u64, drop_count: &mut u64) {
    let Some(writer) = rtc.writer(mid) else {
        *drop_count += 1;
        if *drop_count <= 3 || *drop_count % 100 == 0 {
            warn!("webrtc: send_video — writer not ready for mid={} (dropped={})", mid, drop_count);
        }
        return;
    };
    // 必须选择 H.264 codec（packetization-mode=1）来匹配 openh264 输出
    let pt = writer.payload_params()
        .filter(|p| p.spec().codec == str0m::format::Codec::H264)
        .find(|p| p.spec().format.packetization_mode == Some(1))
        .or_else(|| writer.payload_params().find(|p| p.spec().codec == str0m::format::Codec::H264))
        .map(|p| p.pt());
    let Some(pt) = pt else {
        warn!("webrtc: send_video — no H.264 payload params for mid={}", mid);
        return;
    };
    *write_count += 1;
    if let Err(e) = writer.write(pt, Instant::now(), MediaTime::from_90khz(ts as i64), data.to_vec()) {
        if *write_count <= 5 || *write_count % 300 == 0 {
            warn!("webrtc: send_video — write error: {} (count={})", e, write_count);
        }
    }
}

fn send_audio(rtc: &mut Rtc, mid: Mid, data: &[u8], ts: u32) {
    let Some(writer) = rtc.writer(mid) else { return };
    let pt = writer.payload_params().next().map(|p| p.pt());
    let Some(pt) = pt else { return };
    let _ = writer.write(pt, Instant::now(), MediaTime::new(ts as i64, Frequency::FORTY_EIGHT_KHZ), data.to_vec());
}

/// 返回 true 表示应退出主循环（ICE 已失败/断开）
async fn handle_event(event: Event, hid_tx: &Sender<Bytes>, ice_connected: &mut bool, keyframe_flag: &Arc<AtomicBool>, peer_connected: &Arc<AtomicBool>) -> bool {
    match event {
        Event::Connected => {
            // Both ICE and DTLS are fully established — safe to send media now
            info!("webrtc: ICE+DTLS connected, media can now flow");
            *ice_connected = true;
            peer_connected.store(true, Ordering::Relaxed);
            keyframe_flag.store(true, Ordering::Relaxed);
        }
        Event::ChannelData(data) => {
            let _ = hid_tx.send(Bytes::copy_from_slice(&data.data)).await;
        }
        Event::IceConnectionStateChange(s) => {
            info!("ICE connection state: {:?}", s);
            use str0m::IceConnectionState;
            match s {
                IceConnectionState::Connected | IceConnectionState::Completed => {
                    // ICE is ready but DTLS may not be done yet — pre-request IDR so encoder
                    // has a keyframe ready by the time Event::Connected fires
                    info!("webrtc: ICE connected, pre-requesting IDR keyframe");
                    keyframe_flag.store(true, Ordering::Relaxed);
                }
                IceConnectionState::Disconnected => {
                    if *ice_connected {
                        warn!("ICE disconnected after established session, exiting loop");
                        peer_connected.store(false, Ordering::Relaxed);
                        return true;
                    }
                }
                _ => {}
            }
        }
        Event::KeyframeRequest(_) => {
            debug!("webrtc: KeyframeRequest received, signaling encoder");
            keyframe_flag.store(true, Ordering::Relaxed);
        }
        other => {
            debug!("webrtc event: {:?}", other);
        }
    }
    false
}

/// 解析 Annex B 字节流，返回 NAL 类型描述（用于调试日志）
fn describe_h264_nals(data: &[u8]) -> String {
    let mut types = Vec::new();
    let mut i = 0;
    while i < data.len() {
        // 查找 00 00 01 或 00 00 00 01 start code
        if i + 3 < data.len() && data[i] == 0 && data[i+1] == 0 {
            let (nal_start, _sc_len) = if data[i+2] == 1 {
                (i + 3, 3)
            } else if i + 4 <= data.len() && data[i+2] == 0 && data[i+3] == 1 {
                (i + 4, 4)
            } else {
                i += 1;
                continue;
            };
            if nal_start < data.len() {
                let nal_type = data[nal_start] & 0x1F;
                let name = match nal_type {
                    1 => "P-slice",
                    5 => "IDR",
                    6 => "SEI",
                    7 => "SPS",
                    8 => "PPS",
                    9 => "AUD",
                    _ => "other",
                };
                types.push(format!("{}({})", name, nal_type));
            }
            i = nal_start + 1;
        } else {
            i += 1;
        }
    }
    if types.is_empty() {
        "none".to_string()
    } else {
        types.join(", ")
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

// ─── STUN Binding（RFC 5389）─ 发现公网 IP:port ──────────────────────────────

const STUN_MAGIC: u32 = 0x2112A442;

/// 向 STUN 服务器发送 Binding Request，返回 server-reflexive 地址
async fn stun_binding(socket: &UdpSocket, stun_url: &str) -> Result<SocketAddr> {
    // 解析 "stun:host:port" 或 "stun:host" 格式
    let addr_str = stun_url.strip_prefix("stun:").unwrap_or(stun_url);
    let host_port = if addr_str.contains(':') {
        addr_str.to_string()
    } else {
        format!("{}:3478", addr_str)
    };
    let stun_addr: SocketAddr = tokio::net::lookup_host(&host_port)
        .await
        .context("resolve STUN server")?
        .find(|a| a.is_ipv4()) // UDP socket 绑定在 0.0.0.0，只能用 IPv4
        .context("STUN server has no IPv4 address")?;
    debug!("stun: sending binding request to {}", stun_addr);

    // 构造 20 字节 STUN Binding Request
    let mut req = [0u8; 20];
    req[0] = 0x00; req[1] = 0x01; // Binding Request
    // Message Length = 0
    req[4..8].copy_from_slice(&STUN_MAGIC.to_be_bytes());
    // Transaction ID（12 字节）
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    req[8..16].copy_from_slice(&nanos.to_be_bytes());
    req[16..20].copy_from_slice(&[0xAB, 0xCD, 0xEF, 0x01]);

    socket.send_to(&req, stun_addr).await?;

    // 等待响应（3 秒超时）
    let mut buf = [0u8; 512];
    let n = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let (n, from) = socket.recv_from(&mut buf).await?;
            if from == stun_addr { return Ok::<usize, anyhow::Error>(n); }
        }
    }).await.context("STUN binding timeout")??;

    parse_stun_response(&buf[..n])
}

fn parse_stun_response(data: &[u8]) -> Result<SocketAddr> {
    if data.len() < 20 { anyhow::bail!("STUN response too short"); }

    let cookie = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    if cookie != STUN_MAGIC { anyhow::bail!("invalid STUN magic cookie"); }

    let msg_type = u16::from_be_bytes([data[0], data[1]]);
    if msg_type != 0x0101 { anyhow::bail!("STUN msg type {:04x}, expected 0x0101", msg_type); }

    let msg_len = u16::from_be_bytes([data[2], data[3]]) as usize;
    let end = (20 + msg_len).min(data.len());

    let mut pos = 20;
    while pos + 4 <= end {
        let attr_type = u16::from_be_bytes([data[pos], data[pos+1]]);
        let attr_len  = u16::from_be_bytes([data[pos+2], data[pos+3]]) as usize;
        if pos + 4 + attr_len > data.len() { break; }
        let val = &data[pos+4..pos+4+attr_len];

        if attr_type == 0x0020 { return parse_xor_mapped(val); }   // XOR-MAPPED-ADDRESS
        if attr_type == 0x0001 { return parse_mapped(val); }       // MAPPED-ADDRESS fallback

        pos += 4 + ((attr_len + 3) & !3); // 按 4 字节对齐
    }
    anyhow::bail!("no MAPPED-ADDRESS in STUN response")
}

fn parse_xor_mapped(val: &[u8]) -> Result<SocketAddr> {
    if val.len() < 8 { anyhow::bail!("XOR-MAPPED-ADDRESS too short"); }
    let family = val[1];
    let x_port = u16::from_be_bytes([val[2], val[3]]);
    let port = x_port ^ (STUN_MAGIC >> 16) as u16;
    if family == 0x01 {
        let x_addr = u32::from_be_bytes([val[4], val[5], val[6], val[7]]);
        Ok(SocketAddr::new(IpAddr::V4(Ipv4Addr::from(x_addr ^ STUN_MAGIC)), port))
    } else {
        anyhow::bail!("unsupported address family {}", family)
    }
}

fn parse_mapped(val: &[u8]) -> Result<SocketAddr> {
    if val.len() < 8 { anyhow::bail!("MAPPED-ADDRESS too short"); }
    let port = u16::from_be_bytes([val[2], val[3]]);
    if val[1] == 0x01 {
        Ok(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(val[4], val[5], val[6], val[7])), port))
    } else {
        anyhow::bail!("unsupported address family {}", val[1])
    }
}

// ─── TURN 客户端（RFC 5766）─ 通过 TURN 服务器中继 UDP 数据 ──────────────────

/// TURN 会话状态
struct TurnState {
    turn_addr: SocketAddr,
    realm:     Vec<u8>,
    nonce:     Vec<u8>,
    username:  String,
    password:  String,
}

/// 生成 STUN/TURN transaction ID（12 字节）
fn gen_txn_id() -> [u8; 12] {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut id = [0u8; 12];
    id[..8].copy_from_slice(&(nanos as u64).to_be_bytes());
    id[8..12].copy_from_slice(&((nanos >> 64) as u32).to_be_bytes());
    id
}

/// 构造 STUN/TURN 消息头（20 字节）
fn stun_header(msg_type: u16, msg_len: u16, txn_id: &[u8; 12]) -> [u8; 20] {
    let mut h = [0u8; 20];
    h[0..2].copy_from_slice(&msg_type.to_be_bytes());
    h[2..4].copy_from_slice(&msg_len.to_be_bytes());
    h[4..8].copy_from_slice(&STUN_MAGIC.to_be_bytes());
    h[8..20].copy_from_slice(txn_id);
    h
}

/// 追加 STUN 属性到 buffer（自动 4 字节对齐 padding）
fn append_attr(buf: &mut Vec<u8>, attr_type: u16, value: &[u8]) {
    buf.extend_from_slice(&attr_type.to_be_bytes());
    buf.extend_from_slice(&(value.len() as u16).to_be_bytes());
    buf.extend_from_slice(value);
    // 4 字节对齐 padding
    let pad = (4 - (value.len() % 4)) % 4;
    for _ in 0..pad { buf.push(0); }
}

/// 追加认证属性（USERNAME, REALM, NONCE, MESSAGE-INTEGRITY）
fn append_auth(buf: &mut Vec<u8>, header: &mut [u8; 20], ts: &TurnState) {
    append_attr(buf, 0x0006, ts.username.as_bytes());  // USERNAME
    append_attr(buf, 0x0014, &ts.realm);               // REALM
    append_attr(buf, 0x0015, &ts.nonce);               // NONCE

    // MESSAGE-INTEGRITY: HMAC-SHA1 over the message (updating length first)
    let msg_len_with_integrity = buf.len() as u16 + 24; // +4 attr header +20 HMAC
    header[2..4].copy_from_slice(&msg_len_with_integrity.to_be_bytes());

    use std::io::Write;
    let mut data_to_hmac = Vec::with_capacity(20 + buf.len());
    data_to_hmac.write_all(header).unwrap();
    data_to_hmac.write_all(buf).unwrap();

    let key = md5_key(&ts.username, &ts.realm, &ts.password);
    let hmac = hmac_sha1(&key, &data_to_hmac);
    append_attr(buf, 0x0008, &hmac); // MESSAGE-INTEGRITY
}

/// MD5(username:realm:password) — TURN long-term credential key
fn md5_key(username: &str, realm: &[u8], password: &str) -> [u8; 16] {
    let realm_str = std::str::from_utf8(realm).unwrap_or("");
    let input = format!("{}:{}:{}", username, realm_str, password);
    md5_hash(input.as_bytes())
}

fn md5_hash(data: &[u8]) -> [u8; 16] {
    // Minimal MD5 implementation (RFC 1321)
    let mut state: [u32; 4] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476];
    let orig_len = data.len();

    // Pad message
    let mut padded = data.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 { padded.push(0); }
    padded.extend_from_slice(&((orig_len as u64 * 8).to_le_bytes()));

    const S: [[u32; 4]; 4] = [
        [7, 12, 17, 22], [5, 9, 14, 20], [4, 11, 16, 23], [6, 10, 15, 21],
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee,
        0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
        0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be,
        0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
        0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa,
        0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
        0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
        0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c,
        0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
        0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05,
        0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
        0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039,
        0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1,
        0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
    ];

    for chunk in padded.chunks(64) {
        let mut m = [0u32; 16];
        for i in 0..16 {
            m[i] = u32::from_le_bytes([chunk[i*4], chunk[i*4+1], chunk[i*4+2], chunk[i*4+3]]);
        }
        let [mut a, mut b, mut c, mut d] = state;
        for i in 0..64 {
            let (f, g) = match i {
                0..=15  => ((b & c) | ((!b) & d), i),
                16..=31 => ((d & b) | ((!d) & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _       => (c ^ (b | (!d)), (7 * i) % 16),
            };
            let temp = d;
            d = c; c = b;
            b = b.wrapping_add(
                (a.wrapping_add(f).wrapping_add(K[i]).wrapping_add(m[g]))
                    .rotate_left(S[i / 16][i % 4])
            );
            a = temp;
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
    }

    let mut result = [0u8; 16];
    for (i, s) in state.iter().enumerate() {
        result[i*4..i*4+4].copy_from_slice(&s.to_le_bytes());
    }
    result
}

/// HMAC-SHA1 (RFC 2104)
fn hmac_sha1(key: &[u8], data: &[u8]) -> [u8; 20] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..20].copy_from_slice(&sha1_hash(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 { ipad[i] ^= k[i]; opad[i] ^= k[i]; }

    let mut inner = Vec::with_capacity(64 + data.len());
    inner.extend_from_slice(&ipad);
    inner.extend_from_slice(data);
    let inner_hash = sha1_hash(&inner);

    let mut outer = Vec::with_capacity(64 + 20);
    outer.extend_from_slice(&opad);
    outer.extend_from_slice(&inner_hash);
    sha1_hash(&outer)
}

/// SHA-1 (FIPS 180-4)
fn sha1_hash(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let orig_len = data.len();
    let mut padded = data.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 { padded.push(0); }
    padded.extend_from_slice(&((orig_len as u64 * 8).to_be_bytes()));

    for chunk in padded.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[i*4], chunk[i*4+1], chunk[i*4+2], chunk[i*4+3]]);
        }
        for i in 16..80 {
            w[i] = (w[i-3] ^ w[i-8] ^ w[i-14] ^ w[i-16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for i in 0..80 {
            let (f, k) = match i {
                0..=19  => ((b & c) | ((!b) & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _       => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a.rotate_left(5)
                .wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(w[i]);
            e = d; d = c; c = b.rotate_left(30); b = a; a = temp;
        }
        h[0] = h[0].wrapping_add(a); h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c); h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }

    let mut result = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        result[i*4..i*4+4].copy_from_slice(&v.to_be_bytes());
    }
    result
}

/// TURN Allocate (RFC 5766 §6) — 获取 relay 地址
async fn turn_allocate(
    socket: &UdpSocket, turn_url: &str, username: &str, password: &str,
) -> Result<(SocketAddr, TurnState)> {
    let addr_str = turn_url.strip_prefix("turn:").unwrap_or(turn_url);
    let host_port = if addr_str.contains(':') {
        addr_str.to_string()
    } else {
        format!("{}:3478", addr_str)
    };
    let turn_addr: SocketAddr = tokio::net::lookup_host(&host_port)
        .await.context("resolve TURN server")?
        .find(|a| a.is_ipv4())
        .context("TURN server has no IPv4 address")?;
    debug!("turn: allocating relay via {}", turn_addr);

    // Step 1: 发送不带认证的 Allocate Request（获取 realm + nonce）
    let txn1 = gen_txn_id();
    let mut attrs1 = Vec::new();
    // REQUESTED-TRANSPORT: UDP (0x11000000)
    append_attr(&mut attrs1, 0x0019, &0x11000000u32.to_be_bytes());
    let hdr1 = stun_header(0x0003, attrs1.len() as u16, &txn1); // Allocate Request
    let mut msg1 = Vec::with_capacity(20 + attrs1.len());
    msg1.extend_from_slice(&hdr1);
    msg1.extend_from_slice(&attrs1);
    socket.send_to(&msg1, turn_addr).await?;

    // 读取 401 错误响应（包含 realm + nonce）
    let mut buf = [0u8; 1024];
    let n = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let (n, from) = socket.recv_from(&mut buf).await?;
            if from == turn_addr { return Ok::<usize, anyhow::Error>(n); }
        }
    }).await.context("TURN allocate step1 timeout")??;

    let (realm, nonce) = parse_turn_error_401(&buf[..n])
        .context("TURN server did not return 401 with realm/nonce")?;
    debug!("turn: got realm={}, nonce len={}", String::from_utf8_lossy(&realm), nonce.len());

    // Step 2: 发送带认证的 Allocate Request
    let ts = TurnState {
        turn_addr,
        realm: realm.clone(),
        nonce: nonce.clone(),
        username: username.to_string(),
        password: password.to_string(),
    };

    let txn2 = gen_txn_id();
    let mut attrs2 = Vec::new();
    append_attr(&mut attrs2, 0x0019, &0x11000000u32.to_be_bytes()); // REQUESTED-TRANSPORT
    append_auth(&mut attrs2, &mut stun_header(0x0003, 0, &txn2), &ts);
    let hdr2 = stun_header(0x0003, attrs2.len() as u16, &txn2);
    let mut msg2 = Vec::with_capacity(20 + attrs2.len());
    msg2.extend_from_slice(&hdr2);
    msg2.extend_from_slice(&attrs2);

    // 修正 MESSAGE-INTEGRITY 计算时已更新的长度
    msg2[2..4].copy_from_slice(&(attrs2.len() as u16).to_be_bytes());
    socket.send_to(&msg2, turn_addr).await?;

    let n = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let (n, from) = socket.recv_from(&mut buf).await?;
            if from == turn_addr { return Ok::<usize, anyhow::Error>(n); }
        }
    }).await.context("TURN allocate step2 timeout")??;

    // 解析 Allocate Success 响应
    let resp = &buf[..n];
    let msg_type = u16::from_be_bytes([resp[0], resp[1]]);
    if msg_type != 0x0103 {
        // 可能是错误响应
        if msg_type == 0x0113 {
            anyhow::bail!("TURN Allocate error response (check credentials)");
        }
        anyhow::bail!("unexpected TURN response type {:04x}", msg_type);
    }

    // 查找 XOR-RELAYED-ADDRESS (0x0016)
    let relay = parse_stun_attr(resp, 0x0016)
        .and_then(|val| parse_xor_mapped(val).ok())
        .context("TURN Allocate response missing XOR-RELAYED-ADDRESS")?;

    Ok((relay, ts))
}

/// 解析 TURN 401 错误响应，提取 REALM 和 NONCE
fn parse_turn_error_401(data: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    if data.len() < 20 { anyhow::bail!("response too short"); }
    let mut realm = None;
    let mut nonce = None;

    let msg_len = u16::from_be_bytes([data[2], data[3]]) as usize;
    let end = (20 + msg_len).min(data.len());
    let mut pos = 20;
    while pos + 4 <= end {
        let attr_type = u16::from_be_bytes([data[pos], data[pos+1]]);
        let attr_len = u16::from_be_bytes([data[pos+2], data[pos+3]]) as usize;
        if pos + 4 + attr_len > data.len() { break; }
        match attr_type {
            0x0014 => realm = Some(data[pos+4..pos+4+attr_len].to_vec()), // REALM
            0x0015 => nonce = Some(data[pos+4..pos+4+attr_len].to_vec()), // NONCE
            _ => {}
        }
        pos += 4 + ((attr_len + 3) & !3);
    }
    match (realm, nonce) {
        (Some(r), Some(n)) => Ok((r, n)),
        _ => anyhow::bail!("missing REALM or NONCE in 401"),
    }
}

/// 从 STUN 消息中提取指定属性值
fn parse_stun_attr(data: &[u8], target_type: u16) -> Option<&[u8]> {
    if data.len() < 20 { return None; }
    let msg_len = u16::from_be_bytes([data[2], data[3]]) as usize;
    let end = (20 + msg_len).min(data.len());
    let mut pos = 20;
    while pos + 4 <= end {
        let attr_type = u16::from_be_bytes([data[pos], data[pos+1]]);
        let attr_len = u16::from_be_bytes([data[pos+2], data[pos+3]]) as usize;
        if pos + 4 + attr_len > data.len() { break; }
        if attr_type == target_type {
            return Some(&data[pos+4..pos+4+attr_len]);
        }
        pos += 4 + ((attr_len + 3) & !3);
    }
    None
}

/// TURN CreatePermission（RFC 5766 §9）
async fn turn_create_permission(
    socket: &UdpSocket, ts: &TurnState, peer_ip: IpAddr,
) -> Result<()> {
    let txn = gen_txn_id();
    let mut attrs = Vec::new();

    // XOR-PEER-ADDRESS
    if let IpAddr::V4(ipv4) = peer_ip {
        let mut xpa = [0u8; 8];
        xpa[1] = 0x01; // IPv4
        let xport = 0u16 ^ (STUN_MAGIC >> 16) as u16;
        xpa[2..4].copy_from_slice(&xport.to_be_bytes());
        let xaddr = u32::from(ipv4) ^ STUN_MAGIC;
        xpa[4..8].copy_from_slice(&xaddr.to_be_bytes());
        append_attr(&mut attrs, 0x0012, &xpa);
    }

    append_auth(&mut attrs, &mut stun_header(0x0008, 0, &txn), ts);
    let hdr = stun_header(0x0008, attrs.len() as u16, &txn); // CreatePermission
    let mut msg = Vec::with_capacity(20 + attrs.len());
    msg.extend_from_slice(&hdr);
    msg.extend_from_slice(&attrs);
    msg[2..4].copy_from_slice(&(attrs.len() as u16).to_be_bytes());

    socket.send_to(&msg, ts.turn_addr).await?;

    // 等待响应（不严格检查，最多等 2 秒）
    let mut buf = [0u8; 512];
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let (_n, from) = socket.recv_from(&mut buf).await?;
            if from == ts.turn_addr {
                let mt = u16::from_be_bytes([buf[0], buf[1]]);
                if mt == 0x0108 { return Ok::<(), anyhow::Error>(()); } // success
                if mt == 0x0118 { anyhow::bail!("CreatePermission error"); }
            }
        }
    }).await;
    Ok(())
}

/// TURN Refresh（保持 allocation 存活）
async fn turn_refresh(socket: &UdpSocket, ts: &TurnState) -> Result<()> {
    let txn = gen_txn_id();
    let mut attrs = Vec::new();
    // LIFETIME: 600 秒
    append_attr(&mut attrs, 0x000D, &600u32.to_be_bytes());
    append_auth(&mut attrs, &mut stun_header(0x0004, 0, &txn), ts);
    let hdr = stun_header(0x0004, attrs.len() as u16, &txn);
    let mut msg = Vec::with_capacity(20 + attrs.len());
    msg.extend_from_slice(&hdr);
    msg.extend_from_slice(&attrs);
    msg[2..4].copy_from_slice(&(attrs.len() as u16).to_be_bytes());
    socket.send_to(&msg, ts.turn_addr).await?;
    debug!("turn: refresh sent");
    Ok(())
}

/// 构造 TURN Send indication（RFC 5766 §10）— 将数据通过 TURN 中继发给远端 peer
fn build_turn_send_indication(peer: SocketAddr, data: &[u8]) -> Vec<u8> {
    let mut attrs = Vec::new();

    // XOR-PEER-ADDRESS
    if let IpAddr::V4(ipv4) = peer.ip() {
        let mut xpa = [0u8; 8];
        xpa[1] = 0x01;
        let xport = peer.port() ^ (STUN_MAGIC >> 16) as u16;
        xpa[2..4].copy_from_slice(&xport.to_be_bytes());
        let xaddr = u32::from(ipv4) ^ STUN_MAGIC;
        xpa[4..8].copy_from_slice(&xaddr.to_be_bytes());
        append_attr(&mut attrs, 0x0012, &xpa); // XOR-PEER-ADDRESS
    }

    // DATA
    append_attr(&mut attrs, 0x0013, data);

    let txn = gen_txn_id();
    let hdr = stun_header(0x0016, attrs.len() as u16, &txn); // Send Indication
    let mut msg = Vec::with_capacity(20 + attrs.len());
    msg.extend_from_slice(&hdr);
    msg.extend_from_slice(&attrs);
    msg
}

/// 解析 TURN Data indication（RFC 5766 §10.4）— 从 TURN 服务器接收中继数据
/// 返回 (peer_addr, data_slice)
fn parse_turn_data(pkt: &[u8]) -> Option<(SocketAddr, &[u8])> {
    if pkt.len() < 20 { return None; }
    let msg_type = u16::from_be_bytes([pkt[0], pkt[1]]);

    // Data Indication = 0x0017
    if msg_type != 0x0017 { return None; }

    let cookie = u32::from_be_bytes([pkt[4], pkt[5], pkt[6], pkt[7]]);
    if cookie != STUN_MAGIC { return None; }

    let msg_len = u16::from_be_bytes([pkt[2], pkt[3]]) as usize;
    let end = (20 + msg_len).min(pkt.len());

    let mut peer_addr: Option<SocketAddr> = None;
    let mut data_range: Option<(usize, usize)> = None;

    let mut pos = 20;
    while pos + 4 <= end {
        let attr_type = u16::from_be_bytes([pkt[pos], pkt[pos+1]]);
        let attr_len = u16::from_be_bytes([pkt[pos+2], pkt[pos+3]]) as usize;
        if pos + 4 + attr_len > pkt.len() { break; }

        match attr_type {
            0x0012 => {
                // XOR-PEER-ADDRESS
                if let Ok(addr) = parse_xor_mapped(&pkt[pos+4..pos+4+attr_len]) {
                    peer_addr = Some(addr);
                }
            }
            0x0013 => {
                // DATA
                data_range = Some((pos + 4, pos + 4 + attr_len));
            }
            _ => {}
        }
        pos += 4 + ((attr_len + 3) & !3);
    }

    match (peer_addr, data_range) {
        (Some(addr), Some((start, end))) => Some((addr, &pkt[start..end])),
        _ => None,
    }
}
