// Any-KVM Signal Server
// 极简 WebSocket 信令服务器，仅负责 SDP/ICE 交换。
// 内存占用 < 5 MB，服务器带宽 < 1 KB/s（仅心跳信令）。
package main

import (
	"encoding/json"
	"flag"
	"log"
	"net/http"
	"sync"
	"time"

	"github.com/gorilla/websocket"
)

// ─── 数据结构 ─────────────────────────────────────────────────────────────────

// SignalMsg 是 device ↔ server ↔ client 之间透明传递的信令消息。
// type 字段：offer / answer / candidate / ping / pong
type SignalMsg struct {
	Type    string          `json:"type"`
	Payload json.RawMessage `json:"payload,omitempty"`
}

// peer 代表一个已连接的 WebSocket 客户端（设备端或浏览器控制端）。
type peer struct {
	conn *websocket.Conn
	send chan []byte
}

func newPeer(conn *websocket.Conn) *peer {
	return &peer{conn: conn, send: make(chan []byte, 64)}
}

// room 表示一个 KVM 会话，最多一个设备 + 一个客户端。
type room struct {
	mu          sync.Mutex
	device      *peer
	client      *peer
	deviceName  string
	connectedAt time.Time
	offerCache  []byte // 缓存设备最近一次 offer，客户端连接时自动回放
}

func (r *room) set(role string, p *peer) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if role == "device" {
		r.device = p
	} else {
		r.client = p
	}
}

func (r *room) clear(role string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if role == "device" {
		r.device = nil
		r.offerCache = nil // 设备断开，清除缓存的 offer
	} else {
		r.client = nil
	}
}

// forward 把消息转发给对端（非阻塞，满缓冲区丢弃并记录日志）。
// 设备端的 offer 会被缓存，客户端稍后连接时自动回放。
func (r *room) forward(fromRole string, msg []byte) {
	r.mu.Lock()

	// 缓存设备端 offer，以便客户端稍后连接时能收到
	if fromRole == "device" {
		var m SignalMsg
		if json.Unmarshal(msg, &m) == nil && m.Type == "offer" {
			r.offerCache = make([]byte, len(msg))
			copy(r.offerCache, msg)
		}
	}

	var target *peer
	if fromRole == "device" {
		target = r.client
	} else {
		target = r.device
	}
	r.mu.Unlock()

	if target == nil {
		return
	}
	select {
	case target.send <- msg:
	default:
		log.Printf("[warn] send buffer full for %s's peer, dropping message", fromRole)
	}
}

func (r *room) isEmpty() bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.device == nil && r.client == nil
}

// ─── 全局 room 注册表 ─────────────────────────────────────────────────────────

var (
	rooms   = make(map[string]*room)
	roomsMu sync.Mutex
)

func getOrCreate(id string) *room {
	roomsMu.Lock()
	defer roomsMu.Unlock()
	r, ok := rooms[id]
	if !ok {
		r = &room{}
		rooms[id] = r
		log.Printf("room[%s] created", id)
	}
	return r
}

func tryDelete(id string) {
	roomsMu.Lock()
	defer roomsMu.Unlock()
	r, ok := rooms[id]
	if ok && r.isEmpty() {
		delete(rooms, id)
		log.Printf("room[%s] removed", id)
	}
}

// ─── WebSocket 配置 ───────────────────────────────────────────────────────────

var upgrader = websocket.Upgrader{
	HandshakeTimeout: 10 * time.Second,
	// 允许所有来源（生产环境可校验 Origin）
	CheckOrigin:    func(r *http.Request) bool { return true },
	ReadBufferSize: 4096,
	WriteBufferSize: 4096,
}

const (
	writeWait      = 10 * time.Second
	pongWait       = 120 * time.Second
	pingPeriod     = 30 * time.Second
	maxMessageSize = 64 * 1024
)

// ─── 写循环（每个连接一个 goroutine，避免并发写冲突）────────────────────────

func writePump(p *peer) {
	ticker := time.NewTicker(pingPeriod)
	defer func() {
		ticker.Stop()
		p.conn.Close()
	}()
	for {
		select {
		case msg, ok := <-p.send:
			p.conn.SetWriteDeadline(time.Now().Add(writeWait))
			if !ok {
				p.conn.WriteMessage(websocket.CloseMessage, []byte{})
				return
			}
			if err := p.conn.WriteMessage(websocket.TextMessage, msg); err != nil {
				return
			}
		case <-ticker.C:
			p.conn.SetWriteDeadline(time.Now().Add(writeWait))
			if err := p.conn.WriteMessage(websocket.PingMessage, nil); err != nil {
				return
			}
		}
	}
}

// ─── WebSocket 主处理器 ───────────────────────────────────────────────────────

func wsHandler(w http.ResponseWriter, r *http.Request) {
	roomID := r.URL.Query().Get("room")
	role := r.URL.Query().Get("role") // "device" | "client"

	if roomID == "" {
		http.Error(w, `{"error":"missing room"}`, http.StatusBadRequest)
		return
	}
	if role != "device" && role != "client" {
		http.Error(w, `{"error":"role must be device or client"}`, http.StatusBadRequest)
		return
	}

	conn, err := upgrader.Upgrade(w, r, nil)
	if err != nil {
		log.Printf("upgrade error: %v", err)
		return
	}

	p := newPeer(conn)
	rm := getOrCreate(roomID)
	rm.set(role, p)
	if role == "device" {
		name := r.URL.Query().Get("name")
		if name == "" {
			name = roomID
		}
		rm.mu.Lock()
		rm.deviceName = name
		rm.connectedAt = time.Now()
		rm.mu.Unlock()
	}
	log.Printf("room[%s] %s connected (%s)", roomID, role, r.RemoteAddr)

	go writePump(p)

	// 客户端连接时，若设备已发送过 offer，立即回放
	if role == "client" {
		rm.mu.Lock()
		cached := rm.offerCache
		rm.mu.Unlock()
		if cached != nil {
			select {
			case p.send <- cached:
				log.Printf("room[%s] replayed cached offer to client", roomID)
			default:
				log.Printf("[warn] room[%s] failed to replay offer (buffer full)", roomID)
			}
		}
	}

	conn.SetReadLimit(maxMessageSize)
	conn.SetReadDeadline(time.Now().Add(pongWait))
	conn.SetPongHandler(func(string) error {
		conn.SetReadDeadline(time.Now().Add(pongWait))
		return nil
	})

	defer func() {
		close(p.send)
		rm.clear(role)
		tryDelete(roomID)
		log.Printf("room[%s] %s disconnected", roomID, role)
	}()

	for {
		_, raw, err := conn.ReadMessage()
		if err != nil {
			if websocket.IsUnexpectedCloseError(err,
				websocket.CloseGoingAway, websocket.CloseNormalClosure) {
				log.Printf("room[%s] %s read error: %v", roomID, role, err)
			}
			return
		}
		conn.SetReadDeadline(time.Now().Add(pongWait))

		// 验证 JSON 格式，防止乱码转发
		var m SignalMsg
		if err := json.Unmarshal(raw, &m); err != nil {
			log.Printf("room[%s] invalid JSON from %s: %v", roomID, role, err)
			continue
		}

		// 透明转发给对端
		rm.forward(role, raw)
	}
}

// ─── 健康检查接口 ─────────────────────────────────────────────────────────────

func healthHandler(w http.ResponseWriter, r *http.Request) {
	roomsMu.Lock()
	n := len(rooms)
	roomsMu.Unlock()
	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(map[string]interface{}{"status": "ok", "rooms": n})
}

// ─── Agent 列表接口：返回当前有设备在线的 room 列表 ────────────────────────────

type AgentInfo struct {
	RoomID      string `json:"room_id"`
	Name        string `json:"name"`
	ConnectedAt string `json:"connected_at"`
}

func agentsHandler(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	// 允许 Web 页面跨域查询（仅 GET，只读接口）
	w.Header().Set("Access-Control-Allow-Origin", "*")
	w.Header().Set("Access-Control-Allow-Methods", "GET, OPTIONS")
	if r.Method == http.MethodOptions {
		w.WriteHeader(http.StatusNoContent)
		return
	}

	roomsMu.Lock()
	agents := make([]AgentInfo, 0, len(rooms))
	for id, rm := range rooms {
		rm.mu.Lock()
		hasDevice := rm.device != nil
		name := rm.deviceName
		connAt := rm.connectedAt
		rm.mu.Unlock()
		if hasDevice {
			agents = append(agents, AgentInfo{
				RoomID:      id,
				Name:        name,
				ConnectedAt: connAt.UTC().Format(time.RFC3339),
			})
		}
	}
	roomsMu.Unlock()

	json.NewEncoder(w).Encode(map[string]interface{}{"agents": agents})
}

// ─── 入口 ─────────────────────────────────────────────────────────────────────

func main() {
	addr := flag.String("addr", ":8080", "listen address (e.g. :8080 or 0.0.0.0:8080)")
	web := flag.String("web", "./web", "static web root directory")
	flag.Parse()

	mux := http.NewServeMux()
	mux.HandleFunc("/ws", wsHandler)
	mux.HandleFunc("/health", healthHandler)
	mux.HandleFunc("/api/agents", agentsHandler)
	mux.Handle("/", http.FileServer(http.Dir(*web)))

	srv := &http.Server{
		Addr:         *addr,
		Handler:      mux,
		ReadTimeout:  15 * time.Second,
		WriteTimeout: 15 * time.Second,
		IdleTimeout:  120 * time.Second,
	}

	log.Printf("Any-KVM signal server listening on %s (web: %s)", *addr, *web)
	if err := srv.ListenAndServe(); err != nil {
		log.Fatalf("fatal: %v", err)
	}
}
