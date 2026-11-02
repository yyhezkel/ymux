package term

// notes.go — agent notes for browser mode (Phase 102, WEB-DESIGN B4).
//
// The desktop's notes.rs, on the daemon: the same Note JSON, the same
// {version, notes} file shape, the same `n_<hex nanos>_<hex counter>` ids, so
// a later sync or import between the two needs no translation. Kept in
// <data dir>/notes.json, written to a tmp file and renamed (Rule #7).
//
// Notes are box-wide, like the desktop's are app-wide; workspace_id is kept
// in the shape because the desktop sends and filters on it, but nothing on
// the daemon assigns one. Every change announces `notes:changed` (no data),
// exactly as the desktop does — clients re-list.
//
// Rule #1 does not stretch to notes: they are text the agent or the user
// wrote ON PURPOSE to be kept. They are still never logged — logs carry ids.

import (
	"encoding/json"
	"errors"
	"fmt"
	"sort"
	"sync"
	"sync/atomic"
	"time"
)

// Note is the desktop's Note (notes.rs), field for field.
type Note struct {
	ID          string `json:"id"`
	CreatedAt   string `json:"created_at"`
	UpdatedAt   string `json:"updated_at"`
	Text        string `json:"text"`
	Tag         string `json:"tag,omitempty"`
	Status      string `json:"status"` // "open" | "done"
	WorkspaceID string `json:"workspace_id,omitempty"`
	PaneID      string `json:"pane_id,omitempty"`
}

type notesFile struct {
	Version int    `json:"version"`
	Notes   []Note `json:"notes"`
}

var errNoNote = errors.New("no such note")

// noteStore is the notes file plus a lock. A nil path (no data dir) keeps
// notes in memory only — tests, and a daemon whose dir is unwritable.
type noteStore struct {
	mu    sync.Mutex
	path  string
	notes []Note
	seq   atomic.Uint64
	now   func() time.Time
}

func newNoteStore(path string) *noteStore {
	s := &noteStore{path: path, now: time.Now}
	if path == "" {
		return s
	}
	var f notesFile
	if loadJSON(path, &f, "notes") {
		s.notes = f.Notes
	}
	return s
}

func (s *noteStore) iso() string { return s.now().UTC().Format(time.RFC3339) }

func (s *noteStore) nextID() string {
	return fmt.Sprintf("n_%x_%x", s.now().UnixNano(), s.seq.Add(1)-1)
}

// saveLocked writes the file atomically (webws.go writeFileAtomic). Caller
// holds mu.
func (s *noteStore) saveLocked() error {
	if s.path == "" {
		return nil
	}
	b, err := json.MarshalIndent(notesFile{Version: 1, Notes: s.notes}, "", "  ")
	if err != nil {
		return err
	}
	return writeFileAtomic(s.path, b)
}

func (s *noteStore) add(text, tag, workspaceID, paneID string) (Note, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	now := s.iso()
	n := Note{ID: s.nextID(), CreatedAt: now, UpdatedAt: now, Text: text, Tag: tag,
		Status: "open", WorkspaceID: workspaceID, PaneID: paneID}
	s.notes = append(s.notes, n)
	if err := s.saveLocked(); err != nil {
		s.notes = s.notes[:len(s.notes)-1]
		return Note{}, err
	}
	return n, nil
}

// update changes the given fields. tag != nil sets it ("" clears).
func (s *noteStore) update(id string, text, tag, status *string) (Note, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	for i := range s.notes {
		if s.notes[i].ID != id {
			continue
		}
		prev := s.notes[i]
		n := &s.notes[i]
		if text != nil {
			n.Text = *text
		}
		if tag != nil {
			n.Tag = *tag
		}
		if status != nil {
			n.Status = *status
		}
		n.UpdatedAt = s.iso()
		if err := s.saveLocked(); err != nil {
			s.notes[i] = prev
			return Note{}, err
		}
		return *n, nil
	}
	return Note{}, errNoNote
}

func (s *noteStore) remove(id string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	for i := range s.notes {
		if s.notes[i].ID == id {
			prev := s.notes
			s.notes = append(append([]Note{}, s.notes[:i]...), s.notes[i+1:]...)
			if err := s.saveLocked(); err != nil {
				s.notes = prev
				return err
			}
			return nil
		}
	}
	return errNoNote
}

// list filters like notes.rs list_filtered: most recently updated first.
func (s *noteStore) list(tag, status, workspaceID string, limit int) []Note {
	s.mu.Lock()
	out := make([]Note, 0, len(s.notes))
	for _, n := range s.notes {
		if (tag == "" || n.Tag == tag) && (status == "" || n.Status == status) &&
			(workspaceID == "" || n.WorkspaceID == workspaceID) {
			out = append(out, n)
		}
	}
	s.mu.Unlock()
	sort.SliceStable(out, func(i, j int) bool { return out[i].UpdatedAt > out[j].UpdatedAt })
	if limit > 0 && len(out) > limit {
		out = out[:limit]
	}
	return out
}

func validNoteStatus(s string) bool { return s == "open" || s == "done" }
