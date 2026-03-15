/**
 * Any-KVM Web Console — app.js
 *
 * 职责：
 *  1. 从信令服务器获取在线 Agent 列表，点击一键连接
 *  2. WebRTC RTCPeerConnection 管理（SDP/ICE 协商）
 *  3. 视频/音频轨道绑定到 <video>
 *  4. 键盘/鼠标事件捕获 → 二进制帧 → DataChannel
 *  5. 连接状态 UI 更新
 *  6. 自适应分辨率/帧率（P2P 高画质，TURN 低带宽，帧率优先）
 *  7. 常用快捷键操作面板
 */

'use strict';

const App = (() => {

    const VERSION = '0.2.1';

    // ─── 内置 STUN 服务器列表（自动使用，无需用户填写）────────────────────────
    const _host = window.location.hostname;
    const BUILTIN_STUN = [
        { urls: 'stun:stun.l.google.com:19302' },
        { urls: 'stun:stun1.l.google.com:19302' },
        { urls: 'stun:stun.cloudflare.com:3478' },
        { urls: 'stun:stun.miwifi.com:3478' },
        { urls: `stun:${_host}:3478` },
        { urls: `turn:${_host}:3478`, username: 'kvmuser', credential: 'anykvm2026' },
    ];

    const _proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    const _port = window.location.port ? `:${window.location.port}` : '';
    const DEFAULT_SIGNAL = `${_proto}//${_host}${_port}/ws`;
    const LS_KEY_SERVER = 'anykvm_server_url';

    // ─── 自适应质量档位 ──────────────────────────────────────────────────────
    const QUALITY_PROFILES = {
        p2p_high:  { width: 1920, height: 1080, fps: 30, label: '1080p@30' },
        p2p:       { width: 1280, height: 720,  fps: 30, label: '720p@30' },
        relay:     { width: 1280, height: 720,  fps: 15, label: '720p@15' },
        relay_low: { width: 720,  height: 480,  fps: 15, label: '480p@15' },
        minimum:   { width: 640,  height: 480,  fps: 10, label: '480p@10' },
    };
    // 帧率优先：当实际 FPS < 目标的 70%，降一档分辨率
    const FPS_DOWNGRADE_RATIO = 0.70;
    // 分辨率降档顺序（帧率优先）
    const RES_LADDER = [
        { width: 1920, height: 1080 },
        { width: 1280, height: 720 },
        { width: 720,  height: 480 },
        { width: 640,  height: 480 },
    ];

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
    const sbHid = $('sb-hid');

    // ─── 状态 ────────────────────────────────────────────────────────────────
    let ws = null;
    let pc = null;
    let dc = null;
    let audioEnabled = false;
    let captureActive = false;
    let statsTimer = null;
    let pingTime = 0;
    let currentRoomId = '';
    let connectionType = 'unknown'; // 'p2p' | 'relay' | 'unknown'
    let adaptiveEnabled = true;
    let currentProfile = null;
    let lastActualFps = 0;
    let fpsCheckCount = 0;
    let manualOverride = false; // 用户手动选了分辨率/帧率后禁用自适应

    // FPS 30 秒均值保底（环形缓冲区，15 个样本 × 2s = 30s）
    const FPS_RING_SIZE = 15;
    const FPS_MIN_USABLE = 10;
    let fpsRing = [];
    let fpsGuardActive = false;

    // HID 设备状态（来自 agent 的 0x11 报文）
    let hidKeyboard = false;
    let hidMouse = false;
    let hidStatusReceived = false;

    const hid = { modifier: 0, keys: new Set(), buttons: 0 };

    // ─── 初始化 ──────────────────────────────────────────────────────────────
    (function init() {
        const saved = localStorage.getItem(LS_KEY_SERVER) || DEFAULT_SIGNAL;
        signalInput.value = saved;
        // 显示版本号
        const verEl = $('app-version');
        if (verEl) verEl.textContent = `v${VERSION}`;
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

    // ─── HID 帧格式（8 字节）────────────────────────────────────────────────

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
        const ax = Math.min(32767, Math.max(0, Math.round((x / remoteVideo.clientWidth) * 32767)));
        const ay = Math.min(32767, Math.max(0, Math.round((y / remoteVideo.clientHeight) * 32767)));
        sendHid(new Uint8Array([0x02, hid.buttons,
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

    // ─── 键码转 HID Usage ID ────────────────────────────────────────────────
    const KEY_MAP = {
        'KeyA': 0x04, 'KeyB': 0x05, 'KeyC': 0x06, 'KeyD': 0x07, 'KeyE': 0x08, 'KeyF': 0x09, 'KeyG': 0x0A, 'KeyH': 0x0B,
        'KeyI': 0x0C, 'KeyJ': 0x0D, 'KeyK': 0x0E, 'KeyL': 0x0F, 'KeyM': 0x10, 'KeyN': 0x11, 'KeyO': 0x12, 'KeyP': 0x13,
        'KeyQ': 0x14, 'KeyR': 0x15, 'KeyS': 0x16, 'KeyT': 0x17, 'KeyU': 0x18, 'KeyV': 0x19, 'KeyW': 0x1A, 'KeyX': 0x1B,
        'KeyY': 0x1C, 'KeyZ': 0x1D,
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
                overlayMsg.textContent = '连接成功，等待视频流…';
                detectConnectionType();
                startStatsLoop();
            } else if (s === 'failed') {
                setBadge('failed', '❌ 连接失败');
                overlayMsg.textContent = 'ICE 连接失败，请检查网络或 TURN 配置';
            } else if (s === 'disconnected') {
                setBadge('connecting', '⚠ 断开，重连中…');
                overlayMsg.textContent = '连接断开，等待恢复…';
            }
        };

        pc.ondatachannel = ({ channel }) => {
            console.log('DataChannel received from agent:', channel.label);
            dc = channel;
            dc.binaryType = 'arraybuffer';
            dc.onopen = () => {
                console.log('DataChannel open');
                updateHidStatusUI();
                // DataChannel 打开后立即应用自适应质量
                if (adaptiveEnabled && !manualOverride) {
                    setTimeout(applyAutoQuality, 500);
                }
            };
            dc.onclose = () => {
                console.log('DataChannel closed');
                updateHidStatusUI();
            };
            dc.onmessage = ({ data }) => {
                // 处理来自 agent 的消息
                if (data instanceof ArrayBuffer) {
                    const buf = new Uint8Array(data);
                    if (buf.length >= 2 && buf[0] === 0x11) {
                        hidStatusReceived = true;
                        hidKeyboard = !!(buf[1] & 0x01);
                        hidMouse = !!(buf[1] & 0x02);
                        console.log(`HID status (device ch): kbd=${hidKeyboard} mouse=${hidMouse}`);
                        updateHidStatusUI();
                    }
                } else {
                    console.log('dc from device:', data);
                }
            };
        };

        return pc;
    }

    // ─── 连接类型检测 + 自适应质量 ──────────────────────────────────────────

    function detectConnectionType() {
        if (!pc) return;
        pc.getStats().then(stats => {
            stats.forEach(r => {
                if (r.type === 'candidate-pair' && r.state === 'succeeded') {
                    const local = stats.get(r.localCandidateId);
                    if (local && local.candidateType === 'relay') {
                        connectionType = 'relay';
                        setBadge('relay', '🔄 TURN 中继');
                    } else {
                        connectionType = 'p2p';
                        setBadge('p2p', '✅ P2P 直连');
                    }
                    // 自适应：根据连接类型设置初始质量
                    if (adaptiveEnabled && !manualOverride) {
                        applyAutoQuality();
                    }
                }
            });
        });
    }

    function applyAutoQuality() {
        const profile = connectionType === 'p2p' ? QUALITY_PROFILES.p2p : QUALITY_PROFILES.relay;
        applyProfile(profile);
    }

    function applyProfile(profile) {
        if (!profile) return;
        currentProfile = profile;
        changeResolutionDirect(profile.width, profile.height);
        changeFpsDirect(profile.fps);

        // 更新 UI 下拉框（不触发 onchange）
        const selRes = $('sel-resolution');
        const selFps = $('sel-fps');
        if (selRes) selRes.value = `${profile.width}x${profile.height}`;
        if (selFps) selFps.value = String(profile.fps);

        console.log(`Adaptive: applied profile ${profile.label} (${connectionType})`);
    }

    function adaptiveFpsCheck(actualFps, targetFps) {
        if (!adaptiveEnabled || manualOverride || !currentProfile) return;
        fpsCheckCount++;
        // 每 5 次检查（10 秒）评估一次
        if (fpsCheckCount < 5) return;
        fpsCheckCount = 0;

        if (actualFps > 0 && targetFps > 0 && actualFps < targetFps * FPS_DOWNGRADE_RATIO) {
            // 帧率不足，降低分辨率
            const curW = currentProfile.width;
            const curIdx = RES_LADDER.findIndex(r => r.width <= curW);
            const nextIdx = curIdx + 1;
            if (nextIdx < RES_LADDER.length) {
                const lower = RES_LADDER[nextIdx];
                console.log(`Adaptive: FPS ${actualFps}/${targetFps} too low, downgrading ${curW} → ${lower.width}`);
                const newProfile = { ...currentProfile, width: lower.width, height: lower.height,
                    label: `${lower.height}p@${currentProfile.fps}` };
                applyProfile(newProfile);
            }
        }
    }

    // ─── FPS 30 秒均值保底（硬限制，即使手动模式也生效）─────────────────────
    function fpsGuardCheck(fps) {
        // 0 表示尚无数据，跳过
        if (fps <= 0) return;
        fpsRing.push(fps);
        if (fpsRing.length > FPS_RING_SIZE) fpsRing.shift();
        // 需要至少 10 个样本（20 秒）才开始判断
        if (fpsRing.length < 10) return;
        const avg = fpsRing.reduce((a, b) => a + b, 0) / fpsRing.length;
        if (avg < FPS_MIN_USABLE && !fpsGuardActive) {
            fpsGuardActive = true;
            console.warn(`FPS guard: 30s avg=${avg.toFixed(1)} < ${FPS_MIN_USABLE}, forcing downgrade`);
            // 降低分辨率和帧率到可用档位
            forceMinimumUsable();
        } else if (avg >= FPS_MIN_USABLE + 2) {
            // 恢复标志（+2 做滞后避免频繁切换）
            fpsGuardActive = false;
        }
    }

    function forceMinimumUsable() {
        // 先尝试降分辨率，如果已经最低则降帧率目标
        const selRes = $('sel-resolution');
        const selFps = $('sel-fps');
        const curW = currentProfile ? currentProfile.width : 1280;
        const curIdx = RES_LADDER.findIndex(r => r.width <= curW);
        const nextIdx = curIdx + 1;
        if (nextIdx < RES_LADDER.length) {
            const lower = RES_LADDER[nextIdx];
            console.log(`FPS guard: lowering resolution ${curW} → ${lower.width}`);
            changeResolutionDirect(lower.width, lower.height);
            if (selRes) selRes.value = `${lower.width}x${lower.height}`;
            if (currentProfile) {
                currentProfile = { ...currentProfile, width: lower.width, height: lower.height };
            }
        } else {
            // 分辨率已最低，降帧率到 10
            const curFps = currentProfile ? currentProfile.fps : 15;
            if (curFps > 10) {
                console.log(`FPS guard: lowering fps ${curFps} → 10`);
                changeFpsDirect(10);
                if (selFps) selFps.value = '10';
                if (currentProfile) {
                    currentProfile = { ...currentProfile, fps: 10 };
                }
            }
        }
        // 清空环形缓冲区，重新采样
        fpsRing.length = 0;
    }

    // ─── HID 状态 UI ─────────────────────────────────────────────────────────
    function updateHidStatusUI() {
        if (!sbHid) return;
        if (!dc || dc.readyState !== 'open') {
            sbHid.textContent = '键鼠: ❌ 未连接';
            sbHid.className = 'sb-hid sb-hid-off';
            return;
        }
        if (!hidStatusReceived) {
            sbHid.textContent = '键鼠: ⏳ 通道就绪';
            sbHid.className = 'sb-hid sb-hid-wait';
            return;
        }
        if (hidKeyboard && hidMouse) {
            sbHid.textContent = '⌨✅ 🖱✅';
            sbHid.className = 'sb-hid sb-hid-ok';
        } else if (hidKeyboard) {
            sbHid.textContent = '⌨✅ 🖱❌';
            sbHid.className = 'sb-hid sb-hid-partial';
        } else if (hidMouse) {
            sbHid.textContent = '⌨❌ 🖱✅';
            sbHid.className = 'sb-hid sb-hid-partial';
        } else {
            sbHid.textContent = '⌨❌ 🖱❌';
            sbHid.className = 'sb-hid sb-hid-off';
        }
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
                    // DataChannel 由 agent 创建，浏览器通过 pc.ondatachannel 接收
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
            if (pkts > 0 && !videoOverlay.classList.contains('hidden')) {
                overlayMsg.textContent = `video pkts:${pkts} decoded:${decoded} dropped:${dropped} bytes:${(bytes / 1024).toFixed(0)}KB fps:${fps} rtt:${rtt}ms`;
            }
            if (pkts > 0 && decoded === 0) {
                console.warn('[diag] Receiving video packets but 0 frames decoded!', { pkts, bytes, decoded, dropped, fps });
            }
            // 自适应帧率检查
            lastActualFps = Number(fps) || 0;
            if (currentProfile) {
                adaptiveFpsCheck(lastActualFps, currentProfile.fps);
            }
            // FPS 30 秒均值保底
            fpsGuardCheck(lastActualFps);
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
        connectionType = 'unknown';
        currentProfile = null;
        manualOverride = false;
        fpsCheckCount = 0;
        fpsRing.length = 0;
        fpsGuardActive = false;
        hidKeyboard = false;
        hidMouse = false;
        hidStatusReceived = false;
        updateHidStatusUI();
    }

    // ─── 公共 API ─────────────────────────────────────────────────────────────

    function connect() { fetchAgents(); }

    function disconnect() {
        reset();
        consolePanel.classList.add('hidden');
        connectPanel.classList.remove('hidden');
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

    // ─── 快捷操作 ────────────────────────────────────────────────────────────

    function sendKeyCombo(modifiers, keyCode, holdMs) {
        if (!dc || dc.readyState !== 'open') {
            console.warn('Quick action: DataChannel not open');
            return;
        }
        holdMs = holdMs || 100;
        // 按下
        sendHid(new Uint8Array([0x01, modifiers, keyCode, 0, 0, 0, 0, 0]));
        // 释放
        setTimeout(() => {
            sendHid(new Uint8Array([0x01, 0, 0, 0, 0, 0, 0, 0]));
        }, holdMs);
    }

    function sendCtrlAltDel() {
        // Ctrl(0x01) + Alt(0x04) = 0x05, Delete = 0x4c
        sendKeyCombo(0x05, 0x4c, 150);
    }

    function sendAltTab() {
        // Alt(0x04), Tab = 0x2b
        sendKeyCombo(0x04, 0x2b, 200);
    }

    function sendAltF4() {
        // Alt(0x04), F4 = 0x3d
        sendKeyCombo(0x04, 0x3d, 150);
    }

    function sendWinKey() {
        // Left Meta(0x08), no key
        sendKeyCombo(0x08, 0, 150);
    }

    function sendCtrlShiftEsc() {
        // Ctrl(0x01) + Shift(0x02) = 0x03, Escape = 0x29
        sendKeyCombo(0x03, 0x29, 150);
    }

    function sendPrintScreen() {
        // No modifier, PrintScreen = 0x46
        sendKeyCombo(0, 0x46, 100);
    }

    function sendCtrlC() {
        // Ctrl(0x01), C = 0x06
        sendKeyCombo(0x01, 0x06, 100);
    }

    function sendCtrlV() {
        // Ctrl(0x01), V = 0x19
        sendKeyCombo(0x01, 0x19, 100);
    }

    function sendCtrlA() {
        // Ctrl(0x01), A = 0x04
        sendKeyCombo(0x01, 0x04, 100);
    }

    function sendCtrlZ() {
        // Ctrl(0x01), Z = 0x1D
        sendKeyCombo(0x01, 0x1D, 100);
    }

    function sendEnter() {
        sendKeyCombo(0, 0x28, 80);
    }

    // ─── 文字输入（逐字符发送按键）──────────────────────────────────────────

    function typeText(text) {
        if (!text || !dc || dc.readyState !== 'open') return;
        const charMap = {
            'a':0x04,'b':0x05,'c':0x06,'d':0x07,'e':0x08,'f':0x09,'g':0x0A,'h':0x0B,
            'i':0x0C,'j':0x0D,'k':0x0E,'l':0x0F,'m':0x10,'n':0x11,'o':0x12,'p':0x13,
            'q':0x14,'r':0x15,'s':0x16,'t':0x17,'u':0x18,'v':0x19,'w':0x1A,'x':0x1B,
            'y':0x1C,'z':0x1D,
            '1':0x1e,'2':0x1f,'3':0x20,'4':0x21,'5':0x22,
            '6':0x23,'7':0x24,'8':0x25,'9':0x26,'0':0x27,
            '\n':0x28, ' ':0x2c, '-':0x2d, '=':0x2e,
            '[':0x2f, ']':0x30, '\\':0x31, ';':0x33,
            "'":0x34, '`':0x35, ',':0x36, '.':0x37, '/':0x38, '\t':0x2b,
        };
        const shiftMap = {
            'A':0x04,'B':0x05,'C':0x06,'D':0x07,'E':0x08,'F':0x09,'G':0x0A,'H':0x0B,
            'I':0x0C,'J':0x0D,'K':0x0E,'L':0x0F,'M':0x10,'N':0x11,'O':0x12,'P':0x13,
            'Q':0x14,'R':0x15,'S':0x16,'T':0x17,'U':0x18,'V':0x19,'W':0x1A,'X':0x1B,
            'Y':0x1C,'Z':0x1D,
            '!':0x1e,'@':0x1f,'#':0x20,'$':0x21,'%':0x22,
            '^':0x23,'&':0x24,'*':0x25,'(':0x26,')':0x27,
            '_':0x2d,'+':0x2e,'{':0x2f,'}':0x30,'|':0x31,
            ':':0x33,'"':0x34,'~':0x35,'<':0x36,'>':0x37,'?':0x38,
        };

        let i = 0;
        const interval = setInterval(() => {
            if (i >= text.length || !dc || dc.readyState !== 'open') {
                clearInterval(interval);
                sendHid(new Uint8Array([0x01, 0, 0, 0, 0, 0, 0, 0]));
                return;
            }
            const ch = text[i];
            let keyCode = charMap[ch];
            let mod = 0;
            if (!keyCode && shiftMap[ch]) {
                keyCode = shiftMap[ch];
                mod = 0x02; // Shift
            }
            if (keyCode) {
                sendHid(new Uint8Array([0x01, mod, keyCode, 0, 0, 0, 0, 0]));
                setTimeout(() => {
                    sendHid(new Uint8Array([0x01, 0, 0, 0, 0, 0, 0, 0]));
                }, 30);
            }
            i++;
        }, 60);
    }

    function promptTypeText() {
        const text = prompt('输入要发送到远端的文本:');
        if (text) typeText(text);
    }

    // ─── 分辨率/帧率控制 ─────────────────────────────────────────────────────

    function changeResolutionDirect(w, h) {
        if (!w || !h || !dc || dc.readyState !== 'open') return;
        sendHid(new Uint8Array([0x10, 0x01,
            (w >> 8) & 0xff, w & 0xff,
            (h >> 8) & 0xff, h & 0xff,
            0, 0]));
        console.log(`Control: resolution → ${w}×${h}`);
    }

    function changeFpsDirect(fps) {
        if (!fps || !dc || dc.readyState !== 'open') return;
        sendHid(new Uint8Array([0x10, 0x02, fps, 0, 0, 0, 0, 0]));
        console.log(`Control: fps → ${fps}`);
    }

    function changeResolution(val) {
        if (!val) return;
        manualOverride = true; // 用户手动选择，禁用自适应
        const [w, h] = val.split('x').map(Number);
        if (!w || !h) return;
        changeResolutionDirect(w, h);
        if (currentProfile) {
            currentProfile = { ...currentProfile, width: w, height: h };
        }
    }

    function changeFps(val) {
        if (!val) return;
        manualOverride = true;
        const fps = parseInt(val, 10);
        if (!fps) return;
        changeFpsDirect(fps);
        if (currentProfile) {
            currentProfile = { ...currentProfile, fps };
        }
    }

    function toggleAdaptive() {
        adaptiveEnabled = !adaptiveEnabled;
        manualOverride = false;
        const btn = $('btn-adaptive');
        if (btn) {
            btn.textContent = adaptiveEnabled ? '🔄 自适应' : '📌 手动';
            btn.title = adaptiveEnabled ? '自适应模式（自动调整画质）' : '手动模式（固定画质）';
        }
        if (adaptiveEnabled && connectionType !== 'unknown') {
            applyAutoQuality();
        }
    }

    return {
        connect, disconnect, toggleFullscreen, toggleAudio,
        sendCtrlAltDel, sendAltTab, sendAltF4, sendWinKey,
        sendCtrlShiftEsc, sendPrintScreen,
        sendCtrlC, sendCtrlV, sendCtrlA, sendCtrlZ, sendEnter,
        promptTypeText, toggleAdaptive,
        fetchAgents, changeResolution, changeFps,
    };

})();
