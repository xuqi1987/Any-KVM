/**
 * Any-KVM Web Console — app.js
 *
 * 职责：
 *  1. 从信令服务器获取在线 Agent 列表，点击一键连接
 *  2. WebRTC RTCPeerConnection 管理（SDP/ICE 协商）
 *  3. 视频/音频轨道绑定到 <video>
 *  4. 键盘/鼠标事件捕获 → 二进制帧 → DataChannel
 *  5. 连接状态 UI 更新
 */

'use strict';

const App = (() => {

    // ─── 内置 STUN 服务器列表（自动使用，无需用户填写）────────────────────────
    // 同时包含国际和国内友好节点，WebRTC 引擎会自动挑选最快的
    // 自建 coturn 地址从 window.location.hostname 自动推导（与信令服务器同机部署）
    const _host = window.location.hostname;
    const BUILTIN_STUN = [
        { urls: 'stun:stun.l.google.com:19302' },
        { urls: 'stun:stun1.l.google.com:19302' },
        { urls: 'stun:stun.cloudflare.com:3478' },
        { urls: 'stun:stun.miwifi.com:3478' },      // 小米，国内友好
        { urls: `stun:${_host}:3478` },               // 自建 coturn STUN
        { urls: `turn:${_host}:3478`, username: 'kvmuser', credential: 'anykvm2026' },
    ];

    // 默认信令服务器：自动从当前页面地址推导（无需硬编码 IP）
    const _proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    const _port = window.location.port ? `:${window.location.port}` : '';
    const DEFAULT_SIGNAL = `${_proto}//${_host}${_port}/ws`;
    const LS_KEY_SERVER = 'anykvm_server_url';

    // ─── DOM 引用 ────────────────────────────────────────────────────────────
    const $ = id => document.getElementById(id);
    const connectPanel = $('connect-panel');
    const consolePanel = $('console-panel');
    const connectError = $('connect-error');
    const remoteVideo = $('remote-video');
    const videoOverlay = $('video-overlay');
    const overlayMsg = $('overlay-msg');
    const mouseCapture = $('mouse-capture');
    const statusBadge = $('status-badge');
    const statsText = $('stats-text');
    const roomLabel = $('room-label');
    const btnAudio = $('btn-audio');
    const btnRefresh = $('btn-refresh');
    const agentList = $('agent-list');
    const agentListHint = $('agent-list-hint');
    const sbIce = $('sb-ice');
    const sbRes = $('sb-resolution');
    const sbFps = $('sb-fps');
    const sbLatency = $('sb-latency');
    const sbHint = $('sb-hint');
    const signalInput = $('signal-url');

    // ─── 状态 ────────────────────────────────────────────────────────────────
    let ws = null;
    let pc = null;
    let dc = null;
    let audioEnabled = false;
    let captureActive = false;
    let statsTimer = null;
    let pingTime = 0;
    let currentRoomId = '';

    const hid = { modifier: 0, keys: new Set(), buttons: 0 };

    // ─── 初始化：读取上次使用的服务器地址 ────────────────────────────────────
    (function init() {
        const saved = localStorage.getItem(LS_KEY_SERVER) || DEFAULT_SIGNAL;
        signalInput.value = saved;
        // 页面加载时自动拉取设备列表
        fetchAgents();
    })();

    // ─── 服务器地址 → HTTP API base url ──────────────────────────────────────
    function wsToHttp(wsUrl) {
        return wsUrl.trim()
            .replace(/^ws:\/\//, 'http://')
            .replace(/^wss:\/\//, 'https://')
            .replace(/\/ws$/, '');
    }

    // ─── 获取 Agent 列表 ─────────────────────────────────────────────────────
    async function fetchAgents() {
        const rawUrl = signalInput.value.trim();
        if (!rawUrl) return;

        localStorage.setItem(LS_KEY_SERVER, rawUrl);
        connectError.textContent = '';
        btnRefresh.classList.add('spin');
        agentListHint.textContent = '获取中…';
        agentList.innerHTML = '';

        try {
            const base = wsToHttp(rawUrl);
            const resp = await fetch(`${base}/api/agents`, { signal: AbortSignal.timeout(5000) });
            if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
            const data = await resp.json();
            renderAgentList(data.agents || []);
        } catch (e) {
            agentListHint.textContent = '无法连接服务器，请检查地址或确认服务正在运行';
            agentListHint.style.color = 'var(--danger)';
            console.warn('fetchAgents error:', e);
        } finally {
            btnRefresh.classList.remove('spin');
        }
    }

    function renderAgentList(agents) {
        agentListHint.style.color = '';
        if (agents.length === 0) {
            agentListHint.textContent = '暂无在线设备，请确认 any-kvm-agent 已在设备上运行';
            agentList.innerHTML = '';
            return;
        }
        agentListHint.textContent = `发现 ${agents.length} 台在线设备：`;
        agentList.innerHTML = '';
        agents.forEach(agent => {
            const connectedAgo = agent.connected_at
                ? (() => {
                    const sec = Math.floor((Date.now() - new Date(agent.connected_at)) / 1000);
                    if (sec < 60) return `${sec}秒前连接`;
                    if (sec < 3600) return `${Math.floor(sec / 60)}分钟前连接`;
                    return `${Math.floor(sec / 3600)}小时前连接`;
                })()
                : '';
            const card = document.createElement('div');
            card.className = 'agent-card';
            card.innerHTML = `
                <span class="agent-icon">💻</span>
                <div class="agent-info">
                    <div class="agent-name">${escHtml(agent.name || agent.room_id)}</div>
                    <div class="agent-meta">房间: ${escHtml(agent.room_id)}${connectedAgo ? '  ·  ' + connectedAgo : ''}</div>
                </div>
                <span class="agent-online" title="在线"></span>
                <button class="btn-connect-agent">连接</button>
            `;
            card.querySelector('.btn-connect-agent').addEventListener('click', () => {
                connectToAgent(agent.room_id);
            });
            agentList.appendChild(card);
        });
    }

    function escHtml(s) {
        return String(s)
            .replace(/&/g, '&amp;').replace(/</g, '&lt;')
            .replace(/>/g, '&gt;').replace(/"/g, '&quot;');
    }

    // ─── 连接到指定 Agent ─────────────────────────────────────────────────────
    function connectToAgent(roomId) {
        const signalUrl = signalInput.value.trim();
        if (!signalUrl) { setError('请填写信令服务器地址'); return; }
        if (!/^wss?:\/\//.test(signalUrl)) {
            setError('地址格式错误，应以 ws:// 或 wss:// 开头');
            return;
        }
        connectError.textContent = '';
        currentRoomId = roomId;
        reset();
        showConsole(roomId);
        connectSignal(signalUrl, roomId);
    }

    // ─── HID 帧格式（8 字节，对应设备端 hid.rs 协议）────────────────────────
    //  type=0x01 键盘  : [0x01, modifier, key1, key2, key3, key4, key5, key6]
    //  type=0x02 鼠标移动: [0x02, 0, ax_hi, ax_lo, ay_hi, ay_lo, 0, 0]  绝对坐标 0-32767
    //  type=0x03 鼠标按键: [0x03, buttons, 0, 0, 0, 0, 0, 0]
    //  type=0x04 鼠标滚轮: [0x04, delta&0xff, 0, 0, 0, 0, 0, 0]

    function sendHid(buf) {
        if (dc && dc.readyState === 'open') {
            dc.send(buf);
        }
    }

    function sendKeyboard() {
        const keys = [...hid.keys].slice(0, 6);
        while (keys.length < 6) keys.push(0);
        sendHid(new Uint8Array([0x01, hid.modifier, ...keys]));
    }

    function sendMouseMove(x, y) {
        // 将像素坐标映射到 0-32767 绝对坐标（与 video 元素实际对应分辨率对齐）
        const ax = Math.round((x / remoteVideo.clientWidth) * 32767) & 0x7fff;
        const ay = Math.round((y / remoteVideo.clientHeight) * 32767) & 0x7fff;
        sendHid(new Uint8Array([0x02, 0,
            (ax >> 8) & 0xff, ax & 0xff,
            (ay >> 8) & 0xff, ay & 0xff,
            0, 0]));
    }

    function sendMouseButtons() {
        sendHid(new Uint8Array([0x03, hid.buttons, 0, 0, 0, 0, 0, 0]));
    }

    function sendMouseWheel(delta) {
        const d = Math.max(-127, Math.min(127, delta)) & 0xff;
        sendHid(new Uint8Array([0x04, d, 0, 0, 0, 0, 0, 0]));
    }

    // ─── 键码转 HID Usage ID（USB HID Keyboard Usage Page 0x07）─────────────
    // 覆盖常用键；完整映射可按需扩展
    const KEY_MAP = {
        'KeyA': 4, 'KeyB': 5, 'KeyC': 6, 'KeyD': 7, 'KeyE': 8, 'KeyF': 9, 'KeyG': 10, 'KeyH': 11,
        'KeyI': 18, 'KeyJ': 19, 'KeyK': 20, 'KeyL': 21, 'KeyM': 22, 'KeyN': 23, 'KeyO': 24, 'KeyP': 25,
        'KeyQ': 26, 'KeyR': 27, 'KeyS': 28, 'KeyT': 29, 'KeyU': 30, 'KeyV': 31, 'KeyW': 32, 'KeyX': 33,
        'KeyY': 34, 'KeyZ': 35,
        'Digit1': 0x1e, 'Digit2': 0x1f, 'Digit3': 0x20, 'Digit4': 0x21, 'Digit5': 0x22,
        'Digit6': 0x23, 'Digit7': 0x24, 'Digit8': 0x25, 'Digit9': 0x26, 'Digit0': 0x27,
        'Enter': 0x28, 'Escape': 0x29, 'Backspace': 0x2a, 'Tab': 0x2b, 'Space': 0x2c,
        'Minus': 0x2d, 'Equal': 0x2e, 'BracketLeft': 0x2f, 'BracketRight': 0x30, 'Backslash': 0x31,
        'Semicolon': 0x33, 'Quote': 0x34, 'Backquote': 0x35, 'Comma': 0x36, 'Period': 0x37, 'Slash': 0x38,
        'CapsLock': 0x39,
        'F1': 0x3a, 'F2': 0x3b, 'F3': 0x3c, 'F4': 0x3d, 'F5': 0x3e, 'F6': 0x3f,
        'F7': 0x40, 'F8': 0x41, 'F9': 0x42, 'F10': 0x43, 'F11': 0x44, 'F12': 0x45,
        'PrintScreen': 0x46, 'ScrollLock': 0x47, 'Pause': 0x48,
        'Insert': 0x49, 'Home': 0x4a, 'PageUp': 0x4b, 'Delete': 0x4c, 'End': 0x4d, 'PageDown': 0x4e,
        'ArrowRight': 0x4f, 'ArrowLeft': 0x50, 'ArrowDown': 0x51, 'ArrowUp': 0x52,
        'NumLock': 0x53, 'NumpadDivide': 0x54, 'NumpadMultiply': 0x55, 'NumpadSubtract': 0x56,
        'NumpadAdd': 0x57, 'NumpadEnter': 0x58,
        'Numpad1': 0x59, 'Numpad2': 0x5a, 'Numpad3': 0x5b, 'Numpad4': 0x5c, 'Numpad5': 0x5d,
        'Numpad6': 0x5e, 'Numpad7': 0x5f, 'Numpad8': 0x60, 'Numpad9': 0x61, 'Numpad0': 0x62,
        'NumpadDecimal': 0x63,
        'ContextMenu': 0x65,
        'ControlLeft': 0, 'ControlRight': 0, 'ShiftLeft': 0, 'ShiftRight': 0,
        'AltLeft': 0, 'AltRight': 0, 'MetaLeft': 0, 'MetaRight': 0,
    };

    function modifierBit(code) {
        switch (code) {
            case 'ControlLeft': return 0x01;
            case 'ShiftLeft': return 0x02;
            case 'AltLeft': return 0x04;
            case 'MetaLeft': return 0x08;
            case 'ControlRight': return 0x10;
            case 'ShiftRight': return 0x20;
            case 'AltRight': return 0x40;
            case 'MetaRight': return 0x80;
            default: return 0;
        }
    }

    // ─── 键盘/鼠标事件处理 ──────────────────────────────────────────────────

    function onKeyDown(e) {
        e.preventDefault();
        const mod = modifierBit(e.code);
        if (mod) {
            hid.modifier |= mod;
        } else {
            const usage = KEY_MAP[e.code];
            if (usage) hid.keys.add(usage);
        }
        sendKeyboard();
    }

    function onKeyUp(e) {
        e.preventDefault();
        const mod = modifierBit(e.code);
        if (mod) {
            hid.modifier &= ~mod;
        } else {
            const usage = KEY_MAP[e.code];
            if (usage) hid.keys.delete(usage);
        }
        sendKeyboard();
    }

    function onMouseMove(e) {
        const rect = mouseCapture.getBoundingClientRect();
        sendMouseMove(e.clientX - rect.left, e.clientY - rect.top);
    }

    function onMouseDown(e) {
        e.preventDefault();
        // 左=0x01, 右=0x02, 中=0x04
        const btn = [0x01, 0x04, 0x02][e.button] || 0;
        hid.buttons |= btn;
        sendMouseButtons();
    }

    function onMouseUp(e) {
        const btn = [0x01, 0x04, 0x02][e.button] || 0;
        hid.buttons &= ~btn;
        sendMouseButtons();
    }

    function onWheel(e) {
        e.preventDefault();
        // deltaY: 正=向下，负=向上；HID wheel: 正=向上（USB HID 规范相反）
        sendMouseWheel(-Math.sign(e.deltaY));
    }

    function onContextMenu(e) { e.preventDefault(); }

    function activateCapture() {
        if (captureActive) return;
        captureActive = true;
        mouseCapture.classList.add('active');
        sbHint.textContent = '键鼠已捕获，按 Esc 释放';
        mouseCapture.addEventListener('mousemove', onMouseMove);
        mouseCapture.addEventListener('mousedown', onMouseDown);
        mouseCapture.addEventListener('mouseup', onMouseUp);
        mouseCapture.addEventListener('wheel', onWheel, { passive: false });
        mouseCapture.addEventListener('contextmenu', onContextMenu);
        document.addEventListener('keydown', onKeyDown);
        document.addEventListener('keyup', onKeyUp);
    }

    function deactivateCapture() {
        if (!captureActive) return;
        captureActive = false;
        mouseCapture.classList.remove('active');
        sbHint.textContent = '点击视频区域激活键鼠控制';
        // 释放所有按键
        hid.modifier = 0; hid.keys.clear(); hid.buttons = 0;
        sendKeyboard(); sendMouseButtons();
        mouseCapture.removeEventListener('mousemove', onMouseMove);
        mouseCapture.removeEventListener('mousedown', onMouseDown);
        mouseCapture.removeEventListener('mouseup', onMouseUp);
        mouseCapture.removeEventListener('wheel', onWheel);
        mouseCapture.removeEventListener('contextmenu', onContextMenu);
        document.removeEventListener('keydown', onKeyDown);
        document.removeEventListener('keyup', onKeyUp);
    }

    mouseCapture.addEventListener('click', activateCapture);
    document.addEventListener('keydown', e => {
        if (e.key === 'Escape' && captureActive) deactivateCapture();
    });

    // ─── ICE / RTCPeerConnection ──────────────────────────────────────────────

    function buildIceServers() {
        const servers = [...BUILTIN_STUN];
        const turnUrl = $('turn-url') ? $('turn-url').value.trim() : '';
        const turnUser = $('turn-user') ? $('turn-user').value.trim() : '';
        const turnPass = $('turn-pass') ? $('turn-pass').value.trim() : '';
        if (turnUrl) {
            servers.push({ urls: turnUrl, username: turnUser, credential: turnPass });
        }
        return servers;
    }

    function createPeerConnection() {
        const iceServers = buildIceServers();
        pc = new RTCPeerConnection({ iceServers });

        // 接收远端 video + audio 轨道
        pc.ontrack = ({ track, streams }) => {
            console.log('ontrack:', track.kind, 'streams:', streams.length);
            const stream = streams[0] || new MediaStream();
            if (track.kind === 'video') {
                stream.addTrack(track);
                remoteVideo.srcObject = stream;
                remoteVideo.play().catch(e => console.warn('video play:', e));
                remoteVideo.onloadedmetadata = () => {
                    videoOverlay.classList.add('hidden');
                    sbRes.textContent = `${remoteVideo.videoWidth}×${remoteVideo.videoHeight}`;
                };
            }
            if (track.kind === 'audio') {
                if (!remoteVideo.srcObject) {
                    stream.addTrack(track);
                    remoteVideo.srcObject = stream;
                } else {
                    remoteVideo.srcObject.addTrack(track);
                }
                if (!audioEnabled) track.enabled = false;
            }
        };

        // 本地 ICE candidate → 发给信令服务器
        pc.onicecandidate = ({ candidate }) => {
            if (candidate) {
                wsSend({ type: 'candidate', payload: candidate });
            }
        };

        pc.oniceconnectionstatechange = () => {
            const s = pc.iceConnectionState;
            console.log('ICE state:', s);
            sbIce.textContent = `ICE: ${s}`;

            if (s === 'connected' || s === 'completed') {
                // ICE 已连通，更新 overlay 状态（即使视频尚未到达）
                overlayMsg.textContent = '连接成功，等待视频流…';
                // 检测是否走 TURN 中继
                pc.getStats().then(stats => {
                    stats.forEach(r => {
                        if (r.type === 'candidate-pair' && r.state === 'succeeded') {
                            const local = stats.get(r.localCandidateId);
                            if (local && local.candidateType === 'relay') {
                                setBadge('relay', '🔄 TURN 中继');
                            } else {
                                setBadge('p2p', '✅ P2P 直连');
                            }
                        }
                    });
                });
                startStatsLoop();
            } else if (s === 'failed') {
                setBadge('failed', '❌ 连接失败');
                overlayMsg.textContent = 'ICE 连接失败，请检查网络或 TURN 配置';
            } else if (s === 'disconnected') {
                setBadge('connecting', '⚠ 断开，重连中…');
                overlayMsg.textContent = '连接断开，等待恢复…';
            }
        };

        // DataChannel：接收设备端 → 客户端消息（当前未使用，预留扩展）
        pc.ondatachannel = ({ channel }) => {
            channel.onmessage = ({ data }) => console.log('dc from device:', data);
        };

        return pc;
    }

    // ─── 信令 WebSocket ────────────────────────────────────────────────────────

    function wsSend(obj) {
        if (ws && ws.readyState === WebSocket.OPEN) {
            ws.send(JSON.stringify(obj));
        }
    }

    function connectSignal(signalUrl, roomId) {
        const url = `${signalUrl}?room=${encodeURIComponent(roomId)}&role=client`;
        console.log('connectSignal: opening', url);
        overlayMsg.textContent = `正在连接 ${url} …`;
        ws = new WebSocket(url);

        ws.onopen = () => {
            console.log('signal connected');
            setBadge('connecting', '⏳ 等待设备…');
            overlayMsg.textContent = '已连接信令，等待设备 offer…';
        };

        ws.onmessage = async ({ data }) => {
            let msg;
            try { msg = JSON.parse(data); } catch { return; }
            console.log('signal msg:', msg.type);

            if (msg.type === 'offer') {
                try {
                    overlayMsg.textContent = '收到 offer，创建 WebRTC…';
                    createPeerConnection();
                    await pc.setRemoteDescription(new RTCSessionDescription(msg.payload));
                    overlayMsg.textContent = '生成 answer…';
                    const answer = await pc.createAnswer();
                    await pc.setLocalDescription(answer);
                    wsSend({ type: 'answer', payload: answer });
                    overlayMsg.textContent = 'ICE 协商中…';

                    // 创建 HID DataChannel（客户端发起）
                    dc = pc.createDataChannel('hid-control', { ordered: false, maxRetransmits: 0 });
                    dc.onopen = () => console.log('DataChannel open');
                    dc.onclose = () => console.log('DataChannel closed');
                } catch (e) {
                    console.error('offer handling error:', e);
                    overlayMsg.textContent = `WebRTC 错误: ${e.message}`;
                }

            } else if (msg.type === 'candidate') {
                if (pc) {
                    try { await pc.addIceCandidate(new RTCIceCandidate(msg.payload)); }
                    catch (e) { console.warn('addIceCandidate error:', e); }
                }
            }
        };

        ws.onerror = (e) => {
            console.error('signal error:', e);
            overlayMsg.textContent = '信令 WebSocket 连接失败';
            setError('信令连接错误，请检查服务器地址');
        };

        ws.onclose = (e) => {
            console.log('signal closed, code:', e.code, 'reason:', e.reason);
            if (overlayMsg.textContent.includes('正在连接')) {
                overlayMsg.textContent = `信令连接关闭 (code=${e.code})`;
            }
        };
    }

    // ─── 统计信息循环 ──────────────────────────────────────────────────────────

    function startStatsLoop() {
        if (statsTimer) return;
        statsTimer = setInterval(async () => {
            if (!pc) return;
            const stats = await pc.getStats();
            let fps = 0, rtt = 0, pkts = 0, bytes = 0, decoded = 0, dropped = 0;
            stats.forEach(r => {
                if (r.type === 'inbound-rtp' && r.kind === 'video') {
                    fps = r.framesPerSecond ? r.framesPerSecond.toFixed(0) : fps;
                    pkts = r.packetsReceived || 0;
                    bytes = r.bytesReceived || 0;
                    decoded = r.framesDecoded || 0;
                    dropped = r.framesDropped || 0;
                }
                if (r.type === 'candidate-pair' && r.state === 'succeeded') {
                    rtt = r.currentRoundTripTime ? (r.currentRoundTripTime * 1000).toFixed(0) : rtt;
                }
            });
            sbFps.textContent = fps ? `${fps} fps` : '';
            sbLatency.textContent = rtt ? `延迟: ${rtt} ms` : '';
            if (pkts > 0) {
                statsText.textContent = `pkts:${pkts} dec:${decoded} drop:${dropped} ${(bytes / 1024).toFixed(0)}KB`;
            }
            // Show stats on overlay when video not playing (debug)
            if (pkts > 0 && !videoOverlay.classList.contains('hidden')) {
                overlayMsg.textContent = `video pkts:${pkts} decoded:${decoded} dropped:${dropped} bytes:${(bytes / 1024).toFixed(0)}KB fps:${fps} rtt:${rtt}ms`;
            }
            // Log detailed stats to console for debugging
            if (pkts > 0 && decoded === 0) {
                console.warn('[diag] Receiving video packets but 0 frames decoded!', { pkts, bytes, decoded, dropped, fps });
            }
        }, 2000);
    }

    function stopStatsLoop() {
        clearInterval(statsTimer);
        statsTimer = null;
    }

    // ─── UI 辅助 ──────────────────────────────────────────────────────────────

    function setBadge(type, text) {
        statusBadge.className = `badge badge-${type}`;
        statusBadge.textContent = text;
    }

    function setError(msg) {
        connectError.textContent = msg;
        btnConnect.disabled = false;
    }

    function showConsole(roomId) {
        connectPanel.classList.add('hidden');
        consolePanel.classList.remove('hidden');
        connectError.textContent = '';
        roomLabel.textContent = `房间: ${roomId}`;
        videoOverlay.classList.remove('hidden');
        overlayMsg.textContent = '连接信令服务器…';
        setBadge('connecting', '连接中…');
    }

    function reset() {
        deactivateCapture();
        stopStatsLoop();
        if (dc) { try { dc.close(); } catch { } dc = null; }
        if (pc) { try { pc.close(); } catch { } pc = null; }
        if (ws) { try { ws.close(); } catch { } ws = null; }
        remoteVideo.srcObject = null;
        hid.modifier = 0; hid.keys.clear(); hid.buttons = 0;
        sbIce.textContent = 'ICE: —';
        sbRes.textContent = '—';
        sbFps.textContent = '—';
        sbLatency.textContent = '延迟: —';
    }

    // ─── 公共 API ─────────────────────────────────────────────────────────────

    function connect() {
        // 兼容旧调用（页面无 room-id 输入框，直接用 fetchAgents 流程）
        fetchAgents();
    }

    function disconnect() {
        reset();
        consolePanel.classList.add('hidden');
        connectPanel.classList.remove('hidden');
        // 断开后自动刷新设备列表
        fetchAgents();
    }

    function toggleFullscreen() {
        if (!document.fullscreenElement) {
            $('video-container').requestFullscreen().catch(console.warn);
        } else {
            document.exitFullscreen().catch(console.warn);
        }
    }

    function toggleAudio() {
        audioEnabled = !audioEnabled;
        btnAudio.textContent = audioEnabled ? '🔊' : '🔇';
        btnAudio.title = audioEnabled ? '关闭音频' : '开启音频';
        if (remoteVideo.srcObject) {
            remoteVideo.srcObject.getAudioTracks().forEach(t => t.enabled = audioEnabled);
        }
        if (audioEnabled) remoteVideo.muted = false;
    }

    function sendCtrlAltDel() {
        if (!dc || dc.readyState !== 'open') return;
        sendHid(new Uint8Array([0x01, 0x01 | 0x04, 0x4c, 0, 0, 0, 0, 0]));
        setTimeout(() => sendHid(new Uint8Array([0x01, 0, 0, 0, 0, 0, 0, 0])), 30);
    }

    return { connect, disconnect, toggleFullscreen, toggleAudio, sendCtrlAltDel, fetchAgents };

})();
