package chat

// Phase 69 — Claude chat session lifecycle. Each session is an independent
// `claude` process spawned with stream-json I/O over pipes (no tmux — see
// docs/PHASE-69-DESIGN.md §3.1). The daemon owns the process; a mobile
// client disconnecting does NOT kill it. Persistence across a daemon restart
// is via Claude's own --resume (the conversation lives under ~/.claude).

import (
	"bufio"
	"context"
	"io"
	"os"
	"os/exec"
	"sync"
	"sync/atomic"
	"time"

	"ymux-server/internal/logging"
)

// logger is the chat subsystem's component logger (Phase 79.D unified format).
var logger = logging.New("SRV:CHAT")

// session status values (mirrored in the SQLite `status` column).
const (
	stStarting     = "starting"
	stActive       = "active"
	stWaitingInput = "waiting_input"
	stWaitingHook  = "waiting_hook"
	stInterrupted  = "interrupted"
	stStopped      = "stopped"
	stKilled       = "killed"
	stError        = "error"
)

// subscriber is one connected WS client's outbound queue.
type subscriber struct {
	id int64
	ch chan []byte
}

// Session is a live Claude process plus its fan-out + hook-bridge state.
type Session struct {
	id       string
	deviceID string
	model    string
	cwd      string
	policy   string // auto | gate | block (default gate)
	rpcToken string // per-session HMAC token for the Phase 66 hook bridge (69.C)

	mgr *SessionManager

	mu              sync.Mutex
	cmd             *exec.Cmd
	stdin           io.WriteCloser
	status          string
	claudeSessionID string
	startedAt       int64
	lastActivityAt  int64
	pendingTool     string // in-flight tool name, "" when none (69.B)

	seq   int64 // monotonically increasing event sequence (atomic)
	subMu sync.Mutex
	subs  map[int64]*subscriber
	subID int64

	// pendingHooks: req_id -> channel the RPC bridge waits on for the
	// mobile client's allow/deny (69.C).
	hookMu       sync.Mutex
	pendingHooks map[string]chan string
}

// SessionManager owns all live sessions and the shared config.
type SessionManager struct {
	store *ChatStore
	cfg   chatConfig

	mu       sync.Mutex
	sessions map[string]*Session

	// rpcAddr/paneIndex are populated by the hook-RPC server (69.C). When
	// rpcAddr is empty, sessions spawn without the YMUX_* hook env (so the
	// chat path is testable before 69.C lands).
	rpcAddr   string
	paneIndex map[string]string // "mob_<sessionID>" -> sessionID

	// beta.3 Fix 3: optional workspace-id → name resolver, injected by the
	// workspace manager via SetWorkspaceResolver so chat_hookrpc can attach
	// a workspace label to emitted hook_request events. Nil in tests.
	wsResolver func(id string) (string, bool)
}

type chatConfig struct {
	claudeBin         string
	baseArgs          []string
	maxPerDevice      int
	maxGlobal         int
	cleanupIdleHours  int
	hookDenyOnTimeout bool
}

func NewSessionManager(store *ChatStore) *SessionManager {
	bin := os.Getenv("YMUX_CLAUDE_BIN")
	if bin == "" {
		bin = os.Getenv("WINMUX_CLAUDE_BIN") // pre-rename spelling, honoured for existing unit files
	}
	if bin == "" {
		if p, err := exec.LookPath("claude"); err == nil {
			bin = p
		} else {
			bin = "claude" // resolved again at spawn; surfaced as an error if absent
		}
	}
	return &SessionManager{
		store:    store,
		sessions: make(map[string]*Session),
		paneIndex: map[string]string{},
		cfg: chatConfig{
			claudeBin: bin,
			// -p with stream-json input keeps the process alive across turns,
			// reading each user message as a JSON line on stdin.
			baseArgs:          []string{"-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose"},
			maxPerDevice:      50,
			maxGlobal:         100,
			cleanupIdleHours:  24,
			hookDenyOnTimeout: true,
		},
	}
}

// RunSessionSweeper periodically kills sessions that are both client-less and
// idle past cleanupIdleHours (docs §9-Q2). Explicit DELETE kills immediately;
// this just reclaims abandoned ones.
func RunSessionSweeper(m *SessionManager, stop <-chan struct{}) {
	t := time.NewTicker(30 * time.Minute)
	defer t.Stop()
	for {
		select {
		case <-stop:
			return
		case <-t.C:
			m.sweepIdle()
		}
	}
}

func (m *SessionManager) sweepIdle() {
	cutoff := time.Now().Add(-time.Duration(m.cfg.cleanupIdleHours) * time.Hour).Unix()
	m.mu.Lock()
	var victims []*Session
	for _, s := range m.sessions {
		s.mu.Lock()
		last := s.lastActivityAt
		s.mu.Unlock()
		if last < cutoff && !s.hasSubscribers() {
			victims = append(victims, s)
		}
	}
	m.mu.Unlock()
	for _, s := range victims {
		s.stop("idle sweep")
		m.forget(s.id)
		logger.Info("swept idle session", "session", s.id)
	}
}

// get returns a live session by id (nil if absent).
func (m *SessionManager) get(id string) *Session {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.sessions[id]
}

// forget removes a session from the live map + pane index. The process must
// already be stopped by the caller.
func (m *SessionManager) forget(id string) {
	m.mu.Lock()
	delete(m.sessions, id)
	delete(m.paneIndex, "mob_"+id)
	m.mu.Unlock()
}

// startSpec is the POST /api/claude/session request.
type startSpec struct {
	Cwd          string `json:"cwd"`
	Model        string `json:"model"`
	SystemPrompt string `json:"system_prompt"`
	DeviceID     string `json:"-"` // filled from the authenticated device
}

// create spawns a new Claude session. Returns the Session or an error
// (rate-limit, spawn failure). The error string is safe to surface.
func (m *SessionManager) create(spec startSpec) (*Session, error) {
	m.mu.Lock()
	if len(m.sessions) >= m.cfg.maxGlobal {
		m.mu.Unlock()
		return nil, errRate("daemon session limit reached")
	}
	m.mu.Unlock()
	if spec.DeviceID != "" && m.store.activeSessionCountForDevice(spec.DeviceID) >= m.cfg.maxPerDevice {
		return nil, errRate("per-device session limit reached")
	}

	now := time.Now().Unix()
	s := &Session{
		id:             "mob_" + randHex(8),
		deviceID:       spec.DeviceID,
		model:          spec.Model,
		cwd:            spec.Cwd,
		policy:         "gate",
		rpcToken:       randHex(32),
		mgr:            m,
		status:         stStarting,
		startedAt:      now,
		lastActivityAt: now,
		subs:           map[int64]*subscriber{},
		pendingHooks:   map[string]chan string{},
	}

	if err := s.spawn(spec.SystemPrompt, ""); err != nil {
		return nil, err
	}

	m.mu.Lock()
	m.sessions[s.id] = s
	m.paneIndex["mob_"+s.id] = s.id
	m.mu.Unlock()

	_ = m.store.insertSession(&SessionRow{
		ID: s.id, DeviceID: s.deviceID, Cwd: s.cwd, Model: s.model,
		Status: stStarting, Policy: s.policy, StartedAt: now, LastActivityAt: now,
	})
	return s, nil
}

// spawn launches the claude process and starts the reader goroutines.
// resumeID, when non-empty, continues a prior conversation (--resume).
func (s *Session) spawn(systemPrompt, resumeID string) error {
	args := append([]string{}, s.mgr.cfg.baseArgs...)
	if s.model != "" {
		args = append(args, "--model", s.model)
	}
	if systemPrompt != "" {
		args = append(args, "--append-system-prompt", systemPrompt)
	}
	if resumeID != "" {
		args = append(args, "--resume", resumeID)
	}

	cmd := exec.Command(s.mgr.cfg.claudeBin, args...) // Rule #3: arg-array, never shell concat
	if s.cwd != "" {
		cmd.Dir = s.cwd
	}
	cmd.Env = s.mgr.spawnEnv(s)

	stdin, err := cmd.StdinPipe()
	if err != nil {
		return err
	}
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return err
	}
	stderr, err := cmd.StderrPipe()
	if err != nil {
		return err
	}
	if err := cmd.Start(); err != nil {
		return err
	}

	s.mu.Lock()
	s.cmd = cmd
	s.stdin = stdin
	s.status = stActive
	s.mu.Unlock()

	go s.readStdout(stdout)
	go s.drainStderr(stderr)
	go s.waitExit()
	return nil
}

// spawnEnv builds the child environment. When the hook-RPC server is up
// (69.C), it injects the Phase 66 trio so Claude's hooks dial back to THIS
// daemon (not the desktop). Token is per-session and never logged (Rule #8).
func (m *SessionManager) spawnEnv(s *Session) []string {
	env := os.Environ()
	if m.rpcAddr != "" {
		// YMUX_* only; the legacy WINMUX_* trio is no longer written.
		env = append(env,
			"YMUX_SOCKET_ADDR="+m.rpcAddr,
			"YMUX_TUNNEL_TOKEN="+s.rpcToken,
			"YMUX_PANE_ID=mob_"+s.id,
		)
	}
	return env
}

// readStdout is replaced by the stream-json parser in 69.B. For 69.A it
// forwards each raw line so the pipeline is provable end-to-end.
func (s *Session) readStdout(r io.Reader) {
	sc := bufio.NewScanner(r)
	sc.Buffer(make([]byte, 0, 64*1024), 8*1024*1024) // claude lines can be large
	for sc.Scan() {
		line := sc.Bytes()
		if len(line) == 0 {
			continue
		}
		s.handleClaudeLine(append([]byte(nil), line...))
	}
}

// drainStderr logs only metadata (line counts), never content (Rule #1).
func (s *Session) drainStderr(r io.Reader) {
	sc := bufio.NewScanner(r)
	n := 0
	for sc.Scan() {
		n++
	}
	if n > 0 {
		logger.Info("session stderr produced output", "session", s.id, "lines", n)
	}
}

// waitExit reaps the process and marks a terminal status.
func (s *Session) waitExit() {
	err := s.cmd.Wait()
	s.mu.Lock()
	prev := s.status
	if prev != stKilled && prev != stStopped {
		if err != nil {
			s.status = stError
		} else {
			s.status = stStopped
		}
	}
	final := s.status
	s.mu.Unlock()
	s.mgr.store.updateSessionStatus(s.id, final)
	s.emit(jsonEvent(map[string]any{"type": "status", "status": final}))
	s.failPendingHooks() // unblock any parked hook waiters → deny
}

func (s *Session) getStatus() string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.status
}

func (s *Session) setStatus(st string) {
	s.mu.Lock()
	s.status = st
	s.lastActivityAt = time.Now().Unix()
	s.mu.Unlock()
	s.mgr.store.updateSessionStatus(s.id, st)
}

// sendUserInput writes one stream-json user message to Claude's stdin.
func (s *Session) sendUserInput(text string) error {
	msg := jsonEvent(map[string]any{
		"type": "user",
		"message": map[string]any{
			"role":    "user",
			"content": []map[string]any{{"type": "text", "text": text}},
		},
	})
	s.mu.Lock()
	w := s.stdin
	s.mu.Unlock()
	if w == nil {
		return errState("session not running")
	}
	if _, err := w.Write(append(msg, '\n')); err != nil {
		return err
	}
	s.setStatus(stActive)
	s.mgr.store.bumpActivity(s.id, 1)
	return nil
}

// interrupt sends SIGINT (Ctrl-C equivalent) to the claude process.
func (s *Session) interrupt() {
	s.mu.Lock()
	p := s.cmd
	s.mu.Unlock()
	if p != nil && p.Process != nil {
		_ = p.Process.Signal(os.Interrupt)
	}
}

// stop ends the session: SIGINT, then SIGTERM after a short grace, then
// SIGKILL. `killed` vs `stopped` is recorded by the caller's intent.
func (s *Session) stop(reason string) {
	s.setStatus(stKilled)
	s.mu.Lock()
	p := s.cmd
	w := s.stdin
	s.mu.Unlock()
	if w != nil {
		_ = w.Close() // closing stdin lets `claude -p` exit cleanly on EOF
	}
	if p == nil || p.Process == nil {
		return
	}
	_ = p.Process.Signal(os.Interrupt)
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), 4*time.Second)
		defer cancel()
		done := make(chan struct{})
		go func() { _, _ = p.Process.Wait(); close(done) }()
		select {
		case <-done:
		case <-ctx.Done():
			_ = p.Process.Kill()
		}
	}()
	logger.Info("session stopped", "session", s.id, "reason", reason)
}

// ─── fan-out ─────────────────────────────────────────────────────────────

func (s *Session) addSubscriber() *subscriber {
	s.subMu.Lock()
	defer s.subMu.Unlock()
	s.subID++
	sub := &subscriber{id: s.subID, ch: make(chan []byte, 256)}
	s.subs[sub.id] = sub
	return sub
}

func (s *Session) removeSubscriber(sub *subscriber) {
	s.subMu.Lock()
	defer s.subMu.Unlock()
	if _, ok := s.subs[sub.id]; ok {
		delete(s.subs, sub.id)
		close(sub.ch)
	}
}

func (s *Session) hasSubscribers() bool {
	s.subMu.Lock()
	defer s.subMu.Unlock()
	return len(s.subs) > 0
}

// emit assigns a sequence, persists to the replay buffer, and fans out to
// every connected client. A slow client drops the event (bounded queue) with
// a logged marker — never blocks the stdout reader (Rule: backpressure-safe).
func (s *Session) emit(event []byte) {
	seq := atomic.AddInt64(&s.seq, 1)
	s.mgr.store.appendReplay(s.id, seq, event)
	s.subMu.Lock()
	for _, sub := range s.subs {
		select {
		case sub.ch <- event:
		default:
			logger.Warn("dropped event for slow client", "session", s.id, "client", sub.id)
		}
	}
	s.subMu.Unlock()
}

func (s *Session) toRow() SessionRow {
	s.mu.Lock()
	defer s.mu.Unlock()
	return SessionRow{
		ID: s.id, DeviceID: s.deviceID, ClaudeSessionID: s.claudeSessionID,
		Cwd: s.cwd, Model: s.model, Status: s.status, Policy: s.policy,
		StartedAt: s.startedAt, LastActivityAt: s.lastActivityAt,
	}
}
