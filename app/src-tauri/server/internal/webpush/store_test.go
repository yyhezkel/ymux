package webpush

import (
	"fmt"
	"os"
	"path/filepath"
	"testing"
)

func rec(dev, ep string) Record {
	return Record{DeviceID: dev, Lang: "en", Sub: Subscription{Endpoint: ep}}
}

func TestStorePutReplacesSameEndpoint(t *testing.T) {
	s := NewStore(filepath.Join(t.TempDir(), "webpush.json"))
	if rs, err := s.All(); err != nil || len(rs) != 0 {
		t.Fatalf("empty store: %v %v", rs, err)
	}
	_ = s.Put(rec("dev_a", "https://p/1"))
	_ = s.Put(rec("dev_a", "https://p/1"))
	_ = s.Put(rec("dev_b", "https://p/2"))
	rs, _ := s.All()
	if len(rs) != 2 {
		t.Fatalf("got %d records, want 2", len(rs))
	}
	st, err := os.Stat(s.path)
	if err != nil {
		t.Fatal(err)
	}
	if st.Mode().Perm() != 0o600 {
		t.Fatalf("file mode %v, want 0600", st.Mode().Perm())
	}
}

func TestStoreRemoveIsPerDevice(t *testing.T) {
	s := NewStore(filepath.Join(t.TempDir(), "webpush.json"))
	_ = s.Put(rec("dev_a", "https://p/1"))
	if ok, _ := s.Remove("https://p/1", "dev_b"); ok {
		t.Fatal("another device removed dev_a's subscription")
	}
	if ok, _ := s.Remove("https://p/1", "dev_a"); !ok {
		t.Fatal("the owning device could not remove it")
	}
	_ = s.Put(rec("dev_a", "https://p/1"))
	if ok, _ := s.Remove("https://p/1", ""); !ok {
		t.Fatal("prune (no device) did not remove it")
	}
}

func TestStoreIsBounded(t *testing.T) {
	s := NewStore(filepath.Join(t.TempDir(), "webpush.json"))
	for i := 0; i < maxRecords+5; i++ {
		_ = s.Put(rec("dev", fmt.Sprintf("https://p/%d", i)))
	}
	rs, _ := s.All()
	if len(rs) != maxRecords || rs[0].Sub.Endpoint != "https://p/5" {
		t.Fatalf("len %d, first %q", len(rs), rs[0].Sub.Endpoint)
	}
}
