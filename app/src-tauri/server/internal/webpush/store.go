package webpush

// store.go — the subscriptions, one JSON file in the data dir.
//
// A handful of records per box (one per browser that said yes), read on every
// send and written on subscribe/unsubscribe/prune, so a file beats a table:
// nothing to migrate, and `cat` shows what the daemon will push to. Written
// tmp + rename, 0600 — an endpoint plus its keys is what a sender needs
// besides our VAPID key, so it is not world-readable.

import (
	"encoding/json"
	"errors"
	"os"
	"sync"
)

// maxRecords bounds the file: a browser that re-subscribes with a fresh
// endpoint each time must not grow it forever. The oldest go first.
const maxRecords = 64

// Record is one browser that subscribed.
type Record struct {
	DeviceID  string       `json:"device_id"` // the paired device, or "owner"
	Lang      string       `json:"lang"`      // "he" | "en": the notification text
	Sub       Subscription `json:"subscription"`
	CreatedMs int64        `json:"created_ms"`
}

// Store is the subscriptions file.
type Store struct {
	path string
	mu   sync.Mutex
}

// NewStore opens (lazily) the file at path.
func NewStore(path string) *Store { return &Store{path: path} }

func (s *Store) load() ([]Record, error) {
	b, err := os.ReadFile(s.path)
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	var out []Record
	if err := json.Unmarshal(b, &out); err != nil {
		return nil, err
	}
	return out, nil
}

func (s *Store) save(rs []Record) error {
	if rs == nil {
		rs = []Record{}
	}
	b, err := json.MarshalIndent(rs, "", "  ")
	if err != nil {
		return err
	}
	return writeAtomic(s.path, b)
}

// Put adds a record, replacing any with the same endpoint (a browser
// re-subscribing on every load is the normal case, not a duplicate).
func (s *Store) Put(r Record) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	rs, err := s.load()
	if err != nil {
		return err
	}
	out := rs[:0]
	for _, x := range rs {
		if x.Sub.Endpoint != r.Sub.Endpoint {
			out = append(out, x)
		}
	}
	out = append(out, r)
	if len(out) > maxRecords {
		out = out[len(out)-maxRecords:]
	}
	return s.save(out)
}

// Remove drops the record for endpoint. When deviceID is not "", only that
// device's record goes — a browser may unsubscribe itself, not another one.
// It reports whether anything was removed.
func (s *Store) Remove(endpoint, deviceID string) (bool, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	rs, err := s.load()
	if err != nil {
		return false, err
	}
	out := rs[:0]
	removed := false
	for _, x := range rs {
		if x.Sub.Endpoint == endpoint && (deviceID == "" || x.DeviceID == deviceID) {
			removed = true
			continue
		}
		out = append(out, x)
	}
	if !removed {
		return false, nil
	}
	return true, s.save(out)
}

// All returns a copy of every record.
func (s *Store) All() ([]Record, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.load()
}
