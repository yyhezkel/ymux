package workspace

// store.go — SQLite persistence for workspaces / sessions / events / pending
// requests / subscribers. A single open connection (SetMaxOpenConns(1))
// serializes all access, which keeps the per-session event `seq` race-free.

import (
	"database/sql"
	"encoding/json"
	"errors"
	"time"

	_ "modernc.org/sqlite" // CGO-free driver
)

// ErrNotFound is returned when a workspace/session id doesn't exist.
var ErrNotFound = errors.New("not found")

// Store is the workspace SQLite store.
type Store struct {
	db *sql.DB
}

// OpenStore opens (creating the schema) the workspace database at path.
func OpenStore(path string) (*Store, error) {
	db, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, err
	}
	db.SetMaxOpenConns(1) // serialize writes → monotonic seq without races
	schema := []string{
		`CREATE TABLE IF NOT EXISTS workspaces (id TEXT PRIMARY KEY, name TEXT, created_at INTEGER)`,
		`CREATE TABLE IF NOT EXISTS sessions (id TEXT PRIMARY KEY, workspace_id TEXT, kind TEXT, created_at INTEGER, last_activity INTEGER)`,
		`CREATE TABLE IF NOT EXISTS events (session_id TEXT, seq INTEGER, type TEXT, ts INTEGER, payload BLOB, PRIMARY KEY(session_id, seq))`,
		`CREATE TABLE IF NOT EXISTS pending_requests (req_id TEXT PRIMARY KEY, session_id TEXT, type TEXT, created_at INTEGER, timeout_at INTEGER, resolved_by TEXT, resolution TEXT)`,
		`CREATE TABLE IF NOT EXISTS subscribers (session_id TEXT, client_id TEXT, device_name TEXT, connected_at INTEGER, last_seen INTEGER, PRIMARY KEY(session_id, client_id))`,
	}
	for _, s := range schema {
		if _, err := db.Exec(s); err != nil {
			_ = db.Close()
			return nil, err
		}
	}
	return &Store{db: db}, nil
}

// Close closes the database.
func (s *Store) Close() { _ = s.db.Close() }

// ─── workspaces ──────────────────────────────────────────────────────────────

func (s *Store) CreateWorkspace(w Workspace) error {
	_, err := s.db.Exec(`INSERT OR REPLACE INTO workspaces(id,name,created_at) VALUES(?,?,?)`,
		w.ID, w.Name, w.CreatedAt)
	return err
}

func (s *Store) ListWorkspaces() ([]Workspace, error) {
	rows, err := s.db.Query(`SELECT id,name,created_at FROM workspaces ORDER BY created_at`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []Workspace{}
	for rows.Next() {
		var w Workspace
		if err := rows.Scan(&w.ID, &w.Name, &w.CreatedAt); err == nil {
			out = append(out, w)
		}
	}
	return out, rows.Err()
}

func (s *Store) GetWorkspace(id string) (Workspace, error) {
	var w Workspace
	err := s.db.QueryRow(`SELECT id,name,created_at FROM workspaces WHERE id=?`, id).
		Scan(&w.ID, &w.Name, &w.CreatedAt)
	if errors.Is(err, sql.ErrNoRows) {
		return w, ErrNotFound
	}
	return w, err
}

// DeleteWorkspace cascades: its sessions, their events + pending + subscribers.
func (s *Store) DeleteWorkspace(id string) error {
	sessions, _ := s.ListSessions(id)
	for _, sess := range sessions {
		_ = s.DeleteSession(sess.ID)
	}
	_, err := s.db.Exec(`DELETE FROM workspaces WHERE id=?`, id)
	return err
}

// ─── sessions ────────────────────────────────────────────────────────────────

func (s *Store) CreateSession(se Session) error {
	_, err := s.db.Exec(
		`INSERT OR REPLACE INTO sessions(id,workspace_id,kind,created_at,last_activity) VALUES(?,?,?,?,?)`,
		se.ID, se.WorkspaceID, se.Kind, se.CreatedAt, se.LastActivity)
	return err
}

func (s *Store) ListSessions(wsID string) ([]Session, error) {
	rows, err := s.db.Query(
		`SELECT id,workspace_id,kind,created_at,last_activity FROM sessions WHERE workspace_id=? ORDER BY created_at`, wsID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []Session{}
	for rows.Next() {
		var se Session
		if err := rows.Scan(&se.ID, &se.WorkspaceID, &se.Kind, &se.CreatedAt, &se.LastActivity); err == nil {
			out = append(out, se)
		}
	}
	return out, rows.Err()
}

func (s *Store) GetSession(id string) (Session, error) {
	var se Session
	err := s.db.QueryRow(
		`SELECT id,workspace_id,kind,created_at,last_activity FROM sessions WHERE id=?`, id).
		Scan(&se.ID, &se.WorkspaceID, &se.Kind, &se.CreatedAt, &se.LastActivity)
	if errors.Is(err, sql.ErrNoRows) {
		return se, ErrNotFound
	}
	return se, err
}

func (s *Store) TouchSession(id string, ts int64) {
	_, _ = s.db.Exec(`UPDATE sessions SET last_activity=? WHERE id=?`, ts, id)
}

func (s *Store) DeleteSession(id string) error {
	_, _ = s.db.Exec(`DELETE FROM events WHERE session_id=?`, id)
	_, _ = s.db.Exec(`DELETE FROM pending_requests WHERE session_id=?`, id)
	_, _ = s.db.Exec(`DELETE FROM subscribers WHERE session_id=?`, id)
	_, err := s.db.Exec(`DELETE FROM sessions WHERE id=?`, id)
	return err
}

// ─── events (append-only, monotonic seq per session) ─────────────────────────

// AppendEvent assigns the next seq for the session and persists the event,
// returning the stored Event (with seq + ts filled). Serialized by the single
// DB connection so seq never collides.
func (s *Store) AppendEvent(sessionID, typ string, payload json.RawMessage) (Event, error) {
	tx, err := s.db.Begin()
	if err != nil {
		return Event{}, err
	}
	var maxSeq int64
	_ = tx.QueryRow(`SELECT COALESCE(MAX(seq),0) FROM events WHERE session_id=?`, sessionID).Scan(&maxSeq)
	seq := maxSeq + 1
	ts := time.Now().Unix()
	if _, err := tx.Exec(`INSERT INTO events(session_id,seq,type,ts,payload) VALUES(?,?,?,?,?)`,
		sessionID, seq, typ, ts, []byte(payload)); err != nil {
		_ = tx.Rollback()
		return Event{}, err
	}
	if err := tx.Commit(); err != nil {
		return Event{}, err
	}
	return Event{Seq: seq, SessionID: sessionID, Type: typ, Timestamp: ts, Payload: payload}, nil
}

// ReplayEvents returns events with seq > afterSeq, oldest first (limit <= 0 =
// no limit). This is the cursor-based replay-on-attach.
func (s *Store) ReplayEvents(sessionID string, afterSeq int64, limit int) ([]Event, error) {
	q := `SELECT seq,type,ts,payload FROM events WHERE session_id=? AND seq>? ORDER BY seq`
	args := []any{sessionID, afterSeq}
	if limit > 0 {
		q += ` LIMIT ?`
		args = append(args, limit)
	}
	rows, err := s.db.Query(q, args...)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []Event{}
	for rows.Next() {
		e := Event{SessionID: sessionID}
		var payload []byte
		if err := rows.Scan(&e.Seq, &e.Type, &e.Timestamp, &payload); err == nil {
			e.Payload = json.RawMessage(payload)
			out = append(out, e)
		}
	}
	return out, rows.Err()
}

// EventCount returns how many events a session has logged.
func (s *Store) EventCount(sessionID string) int64 {
	var n int64
	_ = s.db.QueryRow(`SELECT COUNT(*) FROM events WHERE session_id=?`, sessionID).Scan(&n)
	return n
}

// PruneEvents drops events older than the unix cutoff (retention).
func (s *Store) PruneEvents(olderThanUnix int64) {
	_, _ = s.db.Exec(`DELETE FROM events WHERE ts < ?`, olderThanUnix)
}

// ─── pending requests (winner-takes-all) ─────────────────────────────────────

func (s *Store) CreatePending(p PendingRequest) error {
	_, err := s.db.Exec(
		`INSERT OR REPLACE INTO pending_requests(req_id,session_id,type,created_at,timeout_at,resolved_by,resolution) VALUES(?,?,?,?,?,?,?)`,
		p.ReqID, p.SessionID, p.Type, p.CreatedAt, p.TimeoutAt, p.ResolvedBy, p.Resolution)
	return err
}

// notExpired is the SQL test for a pending row that can still be answered:
// timeout_at is a unix-seconds deadline, and 0 means "no deadline" (chat's
// bridge and the tests pass 0; the CLI's forward passes now + its wait).
const notExpired = `(timeout_at=0 OR timeout_at>?)`

// ResolvePending atomically records the FIRST answer: the UPDATE only matches
// while resolved_by is still empty, so exactly one caller wins (idempotent).
// A row past its timeout_at is refused — whoever asked has stopped waiting,
// so a late answer must not be recorded as the decision.
func (s *Store) ResolvePending(reqID, clientID, resolution string) (bool, error) {
	res, err := s.db.Exec(
		`UPDATE pending_requests SET resolved_by=?, resolution=? WHERE req_id=? AND resolved_by='' AND `+notExpired,
		clientID, resolution, reqID, time.Now().Unix())
	if err != nil {
		return false, err
	}
	n, _ := res.RowsAffected()
	return n == 1, nil
}

func (s *Store) GetPending(reqID string) (PendingRequest, bool) {
	var p PendingRequest
	err := s.db.QueryRow(
		`SELECT req_id,session_id,type,created_at,timeout_at,resolved_by,resolution FROM pending_requests WHERE req_id=?`, reqID).
		Scan(&p.ReqID, &p.SessionID, &p.Type, &p.CreatedAt, &p.TimeoutAt, &p.ResolvedBy, &p.Resolution)
	return p, err == nil
}

// ListPending returns the session's unanswered requests that can still be
// answered (an expired one is left out, see notExpired).
func (s *Store) ListPending(sessionID string) ([]PendingRequest, error) {
	rows, err := s.db.Query(
		`SELECT req_id,session_id,type,created_at,timeout_at,resolved_by,resolution FROM pending_requests WHERE session_id=? AND resolved_by='' AND `+notExpired+` ORDER BY created_at`,
		sessionID, time.Now().Unix())
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []PendingRequest{}
	for rows.Next() {
		var p PendingRequest
		if err := rows.Scan(&p.ReqID, &p.SessionID, &p.Type, &p.CreatedAt, &p.TimeoutAt, &p.ResolvedBy, &p.Resolution); err == nil {
			out = append(out, p)
		}
	}
	return out, rows.Err()
}
