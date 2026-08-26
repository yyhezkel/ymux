package insights

import "testing"

// Current() must publish a snapshot on the fallback path and then serve the
// cached pointer without re-sampling — the Phase 72.3 guarantee that /current
// never blocks the request goroutine on live collection.
func TestSamplerCurrentCachesAndReuses(t *testing.T) {
	s := NewSampler()
	if s.last.Load() != nil {
		t.Fatal("fresh sampler should have no cached snapshot")
	}

	// Empty cache → one live sample, published to `last`.
	got := s.Current()
	if got == nil {
		t.Fatal("Current returned nil")
	}
	cached := s.last.Load()
	if cached == nil {
		t.Fatal("Sample must publish to the last cache")
	}

	// Subsequent calls return the SAME cached pointer (no fresh sample).
	if a, b := s.Current(), s.Current(); a != b || a != cached {
		t.Fatalf("Current should reuse the cached snapshot pointer (a=%p b=%p cached=%p)", a, b, cached)
	}
}

// A ticker-style Sample(true) publishes a snapshot that Current then serves.
func TestSamplePublishesForCurrent(t *testing.T) {
	s := NewSampler()
	snap := s.Sample(true)
	if snap == nil {
		t.Fatal("Sample returned nil")
	}
	if s.Current() != snap {
		t.Fatal("Current should serve the most recently sampled snapshot")
	}
}

// Phase 86: the docker / top sub-cadences are a call counter, not a clock.
func TestSampleCadenceCounter(t *testing.T) {
	s := NewSampler()
	// Pre-seed so the "nil → force run" escape hatch doesn't mask the modulo.
	s.lastDocker = []DockerContainer{}
	s.lastTop = []ProcInfo{}
	for n := uint64(0); n < 13; n++ {
		s.mu.Lock()
		calls := s.calls
		s.mu.Unlock()
		if calls != n {
			t.Fatalf("call %d: counter is %d", n, calls)
		}
		snap := s.Sample(true)
		if snap == nil {
			t.Fatalf("call %d: nil snapshot", n)
		}
	}
	if s.calls != 13 {
		t.Fatalf("counter should be 13, got %d", s.calls)
	}
	if dockerEvery != 6 || topEvery != 2 {
		t.Fatalf("cadence constants changed: docker=%d top=%d (update docs/vault/server-go.md)", dockerEvery, topEvery)
	}
}

// Off-cadence ticks carry the previous docker list forward instead of a blank.
func TestSampleReusesLastDocker(t *testing.T) {
	s := NewSampler()
	s.calls = 1 // not a docker tick
	s.lastDocker = []DockerContainer{{ID: "abc", State: "running"}, {ID: "def", State: "exited"}}
	snap := s.Sample(false)
	if snap.DockerTotal != 2 || snap.DockerRunning != 1 || len(snap.Docker) != 2 {
		t.Fatalf("cached docker not carried forward: total=%d running=%d", snap.DockerTotal, snap.DockerRunning)
	}
}
