package workspace

import (
	"encoding/json"
	"testing"
	"time"
)

// A reconnect that reuses a client_id replaces the old channel, and the old
// connection's cancel must not tear down the new one.
func TestResubscribeSameClientIDKeepsNewChannel(t *testing.T) {
	m := newMgr(t)
	w, _ := m.CreateWorkspace("p")
	se, _ := m.CreateSession(w.ID, "")

	_, oldCh, cancelOld, err := m.Subscribe(se.ID, "phone", "p", 0)
	if err != nil {
		t.Fatal(err)
	}
	_, newCh, cancelNew, err := m.Subscribe(se.ID, "phone", "p", 0)
	if err != nil {
		t.Fatal(err)
	}
	defer cancelNew()

	// The replaced channel is closed, so the old connection's loop ends.
	select {
	case _, ok := <-oldCh:
		if ok {
			t.Fatal("old channel delivered a frame instead of closing")
		}
	case <-time.After(2 * time.Second):
		t.Fatal("old channel was not closed on replacement")
	}

	cancelOld() // the old connection going away
	cancelOld() // and a second call is harmless
	if n := m.SubscriberCount(se.ID); n != 1 {
		t.Fatalf("subscribers after the old cancel = %d, want 1", n)
	}
	if _, err := m.Publish(se.ID, "tool_use", json.RawMessage(`{"tool_name":"Bash"}`)); err != nil {
		t.Fatal(err)
	}
	if f := drainForType(newCh, "tool_use"); f == nil {
		t.Fatal("the new connection missed a live frame after the old cancel")
	}
}

// The last subscriber leaving removes the session's in-memory fan-out entry.
func TestLastSubscriberLeavingDropsLiveSession(t *testing.T) {
	m := newMgr(t)
	w, _ := m.CreateWorkspace("p")
	se, _ := m.CreateSession(w.ID, "")
	_, _, cancelA, _ := m.Subscribe(se.ID, "A", "a", 0)
	_, _, cancelB, _ := m.Subscribe(se.ID, "B", "b", 0)

	cancelA()
	m.mu.Lock()
	_, still := m.live[se.ID]
	m.mu.Unlock()
	if !still {
		t.Fatal("live session removed while a subscriber remains")
	}
	cancelB()
	m.mu.Lock()
	_, still = m.live[se.ID]
	m.mu.Unlock()
	if still {
		t.Fatal("live session kept after its last subscriber left")
	}

	// A later subscriber still gets live frames (a fresh entry is made).
	_, ch, cancel, err := m.Subscribe(se.ID, "C", "c", 0)
	if err != nil {
		t.Fatal(err)
	}
	defer cancel()
	if _, err := m.Publish(se.ID, "tool_use", json.RawMessage(`{}`)); err != nil {
		t.Fatal(err)
	}
	if f := drainForType(ch, "tool_use"); f == nil {
		t.Fatal("subscriber after a full drain missed a live frame")
	}
}

// Events published while a client subscribes are in the replay or on the
// channel — never in neither. (Before the fix the replay was read before the
// channel was registered, so this failed whenever a publish fell in between.)
func TestSubscribeDuringPublishLosesNothing(t *testing.T) {
	m := newMgr(t)
	w, _ := m.CreateWorkspace("p")
	se, _ := m.CreateSession(w.ID, "")

	const total = 200 // below the 256-frame channel buffer, so nothing is dropped
	done := make(chan error, 1)
	go func() {
		for i := 0; i < total; i++ {
			if _, err := m.Publish(se.ID, "tool_use", json.RawMessage(`{}`)); err != nil {
				done <- err
				return
			}
		}
		done <- nil
	}()

	replay, ch, cancel, err := m.Subscribe(se.ID, "A", "a", 0)
	if err != nil {
		t.Fatal(err)
	}
	defer cancel()
	if err := <-done; err != nil {
		t.Fatal(err)
	}

	seen := map[int64]bool{}
	for _, ev := range replay {
		seen[ev.Seq] = true
	}
drain:
	for {
		select {
		case f := <-ch:
			var probe struct {
				Seq int64 `json:"seq"`
			}
			_ = json.Unmarshal(f, &probe)
			seen[probe.Seq] = true
		default:
			break drain
		}
	}
	for seq := int64(1); seq <= total; seq++ {
		if !seen[seq] {
			t.Fatalf("seq %d is in neither the replay nor the live channel", seq)
		}
	}
}

// A full buffer drops the frame without blocking Publish, and counts it.
func TestFanoutCountsDrops(t *testing.T) {
	m := newMgr(t)
	w, _ := m.CreateWorkspace("p")
	se, _ := m.CreateSession(w.ID, "")
	_, ch, cancel, _ := m.Subscribe(se.ID, "slow", "s", 0)
	defer cancel()

	const total = 300
	for i := 0; i < total; i++ {
		if _, err := m.Publish(se.ID, "tool_use", json.RawMessage(`{}`)); err != nil {
			t.Fatal(err)
		}
	}
	ls := m.liveOf(se.ID, false)
	if ls == nil {
		t.Fatal("no live session")
	}
	ls.mu.Lock()
	dropped := ls.dropped
	ls.mu.Unlock()
	if want := int64(total - cap(ch)); dropped != want {
		t.Fatalf("dropped = %d, want %d", dropped, want)
	}
}

// The deterministic form of the race above. The seam runs between the two
// steps of Subscribe; with the old order (read the replay, then register the
// channel) an event published there was in neither.
func TestEventBetweenSubscribeStepsIsNotLost(t *testing.T) {
	m := newMgr(t)
	w, _ := m.CreateWorkspace("p")
	se, _ := m.CreateSession(w.ID, "")

	var mid int64
	m.betweenSubscribeSteps = func() {
		ev, err := m.Publish(se.ID, "tool_use", json.RawMessage(`{}`))
		if err != nil {
			t.Errorf("publish between the steps: %v", err)
			return
		}
		mid = ev.Seq
	}
	replay, ch, cancel, err := m.Subscribe(se.ID, "A", "a", 0)
	if err != nil {
		t.Fatal(err)
	}
	defer cancel()

	found := false
	for _, ev := range replay {
		if ev.Seq == mid {
			found = true
		}
	}
	select {
	case f := <-ch:
		var probe struct {
			Seq int64 `json:"seq"`
		}
		_ = json.Unmarshal(f, &probe)
		if probe.Seq == mid {
			found = true
		}
	default:
	}
	if !found {
		t.Fatalf("event seq=%d published between the steps reached neither the replay nor the channel", mid)
	}
}
