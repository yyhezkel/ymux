package term

// feed.go — the daemon's feed for browser-created sessions (Phase 101,
// WEB-DESIGN B3).
//
// The desktop's FeedStore (lib.rs) ported to what a browser needs: the last
// feedMaxItems cards in memory, and a one-shot channel per pending approval
// that feed.decide answers. In memory on purpose — the hook registry it hangs
// off is in memory too, and a card about a session whose hook token died with
// the daemon is not actionable. The BROWSER keeps the history (IndexedDB,
// Phase C; Yossi 2026-10-05); this store only hydrates a client that connects.
//
// Card text is rendered in BOTH languages when the card is made, and each
// events subscriber is sent the one it asked for (?lang=). There is no
// settings store on the daemon to read a language from, and two clients in
// two languages is a real case (a phone in Hebrew, a laptop in English).
//
// Rule #1: an item carries the hook payload (prompt, tool input) because the
// card shows it to the user's own client. It is never logged — log lines here
// carry the request id, pane id, subkind and decision only.

import (
	"encoding/json"
	"strings"
	"sync"

	"ymux-server/internal/agent"
)

// feedMaxItems mirrors the desktop's FEED_MAX_ITEMS.
const feedMaxItems = 50

// Item states, the desktop's FeedItemState.
const (
	statePending  = "pending"
	stateAllowed  = "allowed"
	stateDenied   = "denied"
	stateTimedout = "timedout"
	statePassive  = "passive"
)

// FeedItem is one card, field for field the desktop's FeedItem (lib.rs) —
// except workspace_id, which has no meaning on the daemon; `session` (the
// tmux session name) is what a browser groups by instead.
type FeedItem struct {
	RequestID string          `json:"request_id"`
	Kind      string          `json:"kind"`
	Subkind   string          `json:"subkind"`
	PaneID    string          `json:"pane_id,omitempty"`
	Session   string          `json:"session,omitempty"`
	Title     string          `json:"title"`
	Summary   string          `json:"summary"`
	Payload   json.RawMessage `json:"payload"`
	State     string          `json:"state"`
	CreatedMs int64           `json:"created_ms"`
	Blocking  bool            `json:"blocking"`
}

// feedEntry is a stored card: the wire item plus its text in both languages.
type feedEntry struct {
	item               FeedItem
	titleHe, summaryHe string
}

// view renders the card for one subscriber's language.
func (f *feedEntry) view(lang string) FeedItem {
	it := f.item
	if lang == "he" {
		it.Title, it.Summary = f.titleHe, f.summaryHe
	}
	if len(it.Payload) == 0 {
		it.Payload = json.RawMessage("{}")
	}
	return it
}

// feedStore holds the cards and the pending approvals. Its own lock, never
// held while the registry's is taken, so a hook folding and a browser
// deciding never wait on each other.
type feedStore struct {
	mu      sync.Mutex
	items   []*feedEntry
	pending map[string]chan string
}

func newFeedStore() *feedStore {
	return &feedStore{pending: map[string]chan string{}}
}

// add stores a card. A blocking card also gets the channel its decision will
// arrive on; the caller waits on it.
func (s *feedStore) add(e *feedEntry) chan string {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.items = append(s.items, e)
	if over := len(s.items) - feedMaxItems; over > 0 {
		s.items = append(s.items[:0:0], s.items[over:]...)
	}
	if !e.item.Blocking {
		return nil
	}
	ch := make(chan string, 1)
	s.pending[e.item.RequestID] = ch
	return ch
}

// decide resolves a card. Only a PENDING card can be decided, and only once —
// the first decision wins, exactly as the desktop's decide_feed removes the
// sender on first use. Returns false when there was nothing to decide.
func (s *feedStore) decide(requestID, decision string) bool {
	st := map[string]string{"allow": stateAllowed, "deny": stateDenied, "timeout": stateTimedout}[decision]
	if st == "" {
		return false
	}
	s.mu.Lock()
	ch, ok := s.pending[requestID]
	if ok {
		delete(s.pending, requestID)
		for _, e := range s.items {
			if e.item.RequestID == requestID {
				e.item.State = st
			}
		}
	}
	s.mu.Unlock()
	if ok {
		ch <- decision // buffered 1, and the entry is gone: never blocks
	}
	return ok
}

// pendingFor lists the pending request ids of one pane — a killed session's
// approvals are denied rather than left to time out.
func (s *feedStore) pendingFor(paneID string) []string {
	s.mu.Lock()
	defer s.mu.Unlock()
	var out []string
	for _, e := range s.items {
		if e.item.PaneID == paneID && e.item.State == statePending {
			out = append(out, e.item.RequestID)
		}
	}
	return out
}

// viewOf renders one stored card under the lock — decide may be flipping its
// state concurrently.
func (s *feedStore) viewOf(e *feedEntry, lang string) FeedItem {
	s.mu.Lock()
	defer s.mu.Unlock()
	return e.view(lang)
}

// list renders every card for one language, oldest first.
func (s *feedStore) list(lang string) []FeedItem {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := make([]FeedItem, 0, len(s.items))
	for _, e := range s.items {
		out = append(out, e.view(lang))
	}
	return out
}

// cardSubkinds are the lifecycle hooks whose card text is humanized on the
// daemon (rpc_server.rs feed.push: "stop" | "session-end" | … ). pre-tool-use
// is deliberately not here: its gate card's title IS the approval prompt.
var cardSubkinds = map[string]bool{
	"stop": true, "session-end": true, "session-start": true,
	"post-tool-use": true, "subagent-stop": true, "pre-compact": true,
}

// cardText is the desktop's card-text rule for one language. title/summary
// are what the CLI sent; brief is the stop's parsed brief, nil otherwise.
func cardText(subkind, title, summary string, payload map[string]any, brief *agent.Brief, lang string) (string, string) {
	// Phase 110: the CLI derives a gate card's title from payload.command /
	// payload.tool, but Claude Code sends tool_name + tool_input, so it falls
	// back to "agent: pre-tool-use" with the raw hook JSON as the summary.
	// Seen live in the browser. Only that fallback is humanized — a title the
	// CLI did derive is still the approval prompt, untouched.
	if subkind == "pre-tool-use" && strings.HasPrefix(title, "agent: ") {
		return agent.Humanize(subkind, payload, "", lang)
	}
	if !cardSubkinds[subkind] {
		return title, summary
	}
	if brief == nil || brief.Degraded {
		return agent.Humanize(subkind, payload, "", lang)
	}
	// A real brief: never show the raw block on the card — humanize sees
	// the message with it stripped — and prefer the decision line.
	p := make(map[string]any, len(payload))
	for k, v := range payload {
		p[k] = v
	}
	if msg, ok := payload["last_assistant_message"].(string); ok {
		p["last_assistant_message"] = agent.PreBriefText(msg)
	}
	t, s := agent.Humanize(subkind, p, "", lang)
	var decision *string
	switch {
	case brief.Ask != nil && brief.Rec != nil:
		d := *brief.Ask + " · " + *brief.Rec
		decision = &d
	case brief.Ask != nil:
		decision = brief.Ask
	default:
		decision = brief.Delta
	}
	if decision != nil {
		s = agent.ClipChars(*decision, 160)
	}
	return t, s
}
