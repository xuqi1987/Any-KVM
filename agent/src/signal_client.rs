//! signal_client.rs — WebSocket 信令客户端
//!
//! 连接信令服务器，负责：
//!   - 等待 webrtc 模块生成 SDP offer 后发送给服务器
//!   - 接收 SDP answer，转发给 webrtc 模块
//!   - 接收 ICE candidate（远端），转发给 webrtc 模块
//!   - 发送本地 ICE candidate 给服务器
//!   - 断线后自动指数退避重连

use crate::config::SignalConfig;
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, info, warn};
use std::time::Duration;

#[derive(Debug, Serialize, Deserialize)]
struct SignalMsg {
    #[serde(rename = "type")]
    msg_type: String,
    payload:  Value,
}

pub async fn run(
    cfg:              SignalConfig,
    offer_rx:         tokio::sync::oneshot::Receiver<String>,
    answer_tx:        Sender<String>,
    remote_cand_tx:   Sender<String>,
    local_cand_rx:    &mut Receiver<String>,
) -> Result<()> {
    // ─── 先等待 WebRTC 模块生成 offer ────────────────────────────────────────
    let offer_sdp = offer_rx.await.context("webrtc offer channel closed")?;

    // ─── 连接信令服务器（带重连）────────────────────────────────────────────────
    let mut backoff = Duration::from_secs(1);
    loop {
        let url = format!(
            "{}?room={}&role=device",
            cfg.url,
            urlencoding::encode(&cfg.room_id)
        );
        info!("signal: connecting to {}", url);

        match connect_async(&url).await {
            Ok((ws_stream, _)) => {
                info!("signal: connected");
                backoff = Duration::from_secs(1); // 重置退避

                let result = session(
                    ws_stream,
                    &offer_sdp,
                    &answer_tx,
                    &remote_cand_tx,
                    local_cand_rx,
                ).await;

                match result {
                    Ok(()) => {
                        info!("signal: session ended normally");
                        break;
                    }
                    Err(e) => {
                        warn!("signal: session error: {:#}", e);
                    }
                }
            }
            Err(e) => {
                warn!("signal: connect failed: {}", e);
            }
        }

        info!("signal: reconnecting in {:?}…", backoff);
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }

    Ok(())
}

// ─── 单次 WebSocket 会话 ──────────────────────────────────────────────────────

async fn session<S>(
    ws_stream:       S,
    offer_sdp:       &str,
    answer_tx:       &Sender<String>,
    remote_cand_tx:  &Sender<String>,
    local_cand_rx:   &mut Receiver<String>,
) -> Result<()>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
     + futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error>
     + Unpin,
{
    let (mut write, mut read) = ws_stream.split();

    // 发送 SDP offer 给信令服务器（等待客户端连接后转发）
    let offer_msg = json!({
        "type": "offer",
        "payload": {
            "type": "offer",
            "sdp": offer_sdp
        }
    });
    write.send(Message::Text(offer_msg.to_string())).await
        .context("send offer")?;
    info!("signal: SDP offer sent");

    loop {
        tokio::select! {
            // 接收来自信令服务器的消息
            msg = read.next() => {
                match msg {
                    None => {
                        warn!("signal: WebSocket stream ended");
                        return Ok(());
                    }
                    Some(Err(e)) => return Err(e.into()),
                    Some(Ok(Message::Text(raw))) => {
                        handle_incoming(&raw, answer_tx, remote_cand_tx).await;
                    }
                    Some(Ok(Message::Ping(data))) => {
                        write.send(Message::Pong(data)).await.ok();
                    }
                    Some(Ok(Message::Close(_))) => {
                        info!("signal: server closed connection");
                        return Ok(());
                    }
                    _ => {}
                }
            }

            // 发送本地 ICE candidate 给信令服务器
            cand = local_cand_rx.recv() => {
                if let Some(c) = cand {
                    let msg = json!({
                        "type": "candidate",
                        "payload": { "candidate": c, "sdpMid": "0", "sdpMLineIndex": 0 }
                    });
                    debug!("signal: sending local candidate");
                    write.send(Message::Text(msg.to_string())).await
                        .context("send candidate")?;
                }
            }
        }
    }
}

// ─── 处理收到的信令消息 ───────────────────────────────────────────────────────

async fn handle_incoming(
    raw:            &str,
    answer_tx:      &Sender<String>,
    remote_cand_tx: &Sender<String>,
) {
    let Ok(msg) = serde_json::from_str::<SignalMsg>(raw) else {
        warn!("signal: invalid JSON: {}", raw);
        return;
    };

    match msg.msg_type.as_str() {
        "answer" => {
            if let Some(sdp) = msg.payload.get("sdp").and_then(|v| v.as_str()) {
                info!("signal: received SDP answer");
                let _ = answer_tx.send(sdp.to_string()).await;
            } else {
                warn!("signal: answer missing 'sdp' field");
            }
        }
        "candidate" => {
            if let Some(cand) = msg.payload.get("candidate").and_then(|v| v.as_str()) {
                debug!("signal: received remote ICE candidate");
                let _ = remote_cand_tx.send(cand.to_string()).await;
            }
        }
        other => {
            debug!("signal: unknown message type '{}'", other);
        }
    }
}
