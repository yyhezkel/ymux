package agent

// brief.go — port of app/src-tauri/src/brief.rs (the [ymux-brief] parser;
// wire format in docs/BRIEF.md). Change both together.
//
// An agent that cooperates ends its FINAL assistant message with:
//
//	[ymux-brief]
//	task: …
//	status: working | waiting-for-you | stuck | done
//	ask: … / rec: … / next: … / delta: …
//
// The marker is a whole line and the LAST one wins; keys are fixed ASCII
// words split on the FIRST ':' so fully-RTL values cannot confuse it;
// markdown decoration is stripped; unknown keys are ignored. A message with
// no marker degrades to a status-only brief — never fabricate an ask.
//
// Rule #1: brief and prompt CONTENT lives in memory and the UI only.
// Nothing here logs.

import (
	"strings"
	"unicode"
)

const (
	// fieldMaxChars caps every parsed field.
	fieldMaxChars = 200
	// degradedDeltaMaxChars caps the degraded delta (first message line).
	degradedDeltaMaxChars = 160
	// PromptMaxChars caps the user's last prompt kept for the queue.
	PromptMaxChars = 160

	briefMarker = "[ymux-brief]"
)

// BriefStatus is the agent's self-reported status, kebab-case on the wire.
type BriefStatus string

const (
	BriefWorking       BriefStatus = "working"
	BriefWaitingForYou BriefStatus = "waiting-for-you"
	BriefStuck         BriefStatus = "stuck"
	BriefDone          BriefStatus = "done"
)

// ParseBriefStatus is the tolerant parse; aliases cover the phrasings
// agents actually produce. ok is false for anything unrecognised.
func ParseBriefStatus(s string) (BriefStatus, bool) {
	switch asciiLower(strings.TrimSpace(s)) {
	case "working":
		return BriefWorking, true
	case "waiting-for-you", "waiting", "waiting for you":
		return BriefWaitingForYou, true
	case "stuck", "blocked":
		return BriefStuck, true
	case "done":
		return BriefDone, true
	}
	return "", false
}

// Brief is one turn's brief (PaneBrief). Degraded means no marker was found
// and everything is inferred — the UI renders it dimmer and never shows an
// ask. Pointers marshal to null exactly like the Rust Option.
type Brief struct {
	Task      *string     `json:"task"`
	Status    BriefStatus `json:"status"`
	Ask       *string     `json:"ask"`
	Rec       *string     `json:"rec"`
	Next      *string     `json:"next"`
	Delta     *string     `json:"delta"`
	Degraded  bool        `json:"degraded"`
	UpdatedMs int64       `json:"updated_ms"`
}

// BriefEntry is everything known about one pane's conversation beyond the
// traffic light (PaneBriefEntry) — the `pane:brief` event's `entry`.
type BriefEntry struct {
	Brief *Brief `json:"brief"`
	// LastPrompt is the user's last prompt, clipped. Rule #1: display-only.
	LastPrompt   *string `json:"last_prompt"`
	PromptMs     *int64  `json:"prompt_ms"`
	SessionEnded bool    `json:"session_ended"`
	// Seq is bumped on every mutation of the entry; the frontend drops an
	// event whose seq is not newer than what it holds.
	Seq uint32 `json:"seq"`
}

// ParsedBrief is the fields as the agent wrote them. Status is nil when
// absent or unrecognised; the caller defaults it.
type ParsedBrief struct {
	Task, Ask, Rec, Next, Delta *string
	Status                      *BriefStatus
}

// ClipChars trims s and clips it to max characters (runes, not bytes, so a
// Hebrew value is never cut mid-letter), appending an ellipsis when cut.
func ClipChars(s string, max int) string {
	s = strings.TrimSpace(s)
	r := []rune(s)
	if len(r) <= max {
		return s
	}
	return string(r[:max]) + "…"
}

// cleanLine strips the markdown decoration agents wrap lines in: leading
// quote/bullet markers and wrapping emphasis (**bold**, backtick code spans).
func cleanLine(line string) string {
	s := strings.TrimSpace(line)
	for {
		before := s
		s = strings.TrimLeftFunc(strings.TrimLeft(s, ">-*#"), unicode.IsSpace)
		s = strings.TrimSpace(strings.Trim(s, "*`"))
		if s == before {
			return s
		}
	}
}

// markerOffset is the byte offset of the start of the LAST line that is
// exactly the marker, or -1.
func markerOffset(msg string) int {
	found := -1
	pos := 0
	for _, line := range strings.Split(msg, "\n") {
		if asciiEqualFold(cleanLine(line), briefMarker) {
			found = pos
		}
		pos += len(line) + 1
	}
	return found
}

// PreBriefText is the message with the brief block removed — what feed
// cards show instead of the raw block. The whole message when no marker.
func PreBriefText(msg string) string {
	off := markerOffset(msg)
	if off < 0 {
		return msg
	}
	return strings.TrimRightFunc(msg[:off], unicode.IsSpace)
}

// ParseBrief parses the [ymux-brief] block out of an assistant message; ok
// is false when no marker line exists. Unknown keys are skipped; a repeated
// key keeps the LAST value.
func ParseBrief(msg string) (ParsedBrief, bool) {
	var out ParsedBrief
	off := markerOffset(msg)
	if off < 0 {
		return out, false
	}
	lines := strings.Split(msg[off:], "\n")
	for _, line := range lines[1:] { // skip the marker line itself
		line = cleanLine(line)
		if line == "" {
			continue
		}
		key, value, found := strings.Cut(line, ":")
		if !found {
			continue
		}
		value = strings.TrimSpace(value)
		if value == "" {
			continue
		}
		value = ClipChars(value, fieldMaxChars)
		// `- **task**: x` reaches here as `task**: x` — finish the job on
		// the key.
		switch asciiLower(strings.Trim(strings.TrimSpace(key), "*`_")) {
		case "task":
			out.Task = strPtr(value)
		case "status":
			if st, ok := ParseBriefStatus(value); ok {
				out.Status = &st
			}
		case "ask":
			out.Ask = strPtr(value)
		case "rec":
			out.Rec = strPtr(value)
		case "next":
			out.Next = strPtr(value)
		case "delta":
			out.Delta = strPtr(value)
		}
	}
	return out, true
}

// BriefFromStop builds the stored brief for a Stop. lastAssistantMessage
// and autoTitle are nil when absent; a missing message still yields a
// status-only degraded brief (codex/gemini shims send none).
func BriefFromStop(lastAssistantMessage, autoTitle *string, updatedMs int64) Brief {
	var title *string
	if autoTitle != nil {
		title = strPtr(ClipChars(*autoTitle, fieldMaxChars))
	}
	if lastAssistantMessage != nil {
		if p, ok := ParseBrief(*lastAssistantMessage); ok {
			b := Brief{
				Task:      p.Task,
				Status:    BriefDone,
				Ask:       p.Ask,
				Rec:       p.Rec,
				Next:      p.Next,
				Delta:     p.Delta,
				UpdatedMs: updatedMs,
			}
			if b.Task == nil {
				b.Task = title
			}
			if p.Status != nil {
				b.Status = *p.Status
			}
			return b
		}
	}
	b := Brief{Task: title, Status: BriefDone, Degraded: true, UpdatedMs: updatedMs}
	if lastAssistantMessage != nil {
		for _, l := range strings.Split(*lastAssistantMessage, "\n") {
			if l = strings.TrimSpace(l); l != "" {
				b.Delta = strPtr(ClipChars(l, degradedDeltaMaxChars))
				break
			}
		}
	}
	return b
}

func strPtr(s string) *string { return &s }

// asciiLower lowercases ASCII letters only, like Rust's to_ascii_lowercase.
func asciiLower(s string) string {
	b := []byte(s)
	for i, c := range b {
		if 'A' <= c && c <= 'Z' {
			b[i] = c + ('a' - 'A')
		}
	}
	return string(b)
}

// asciiEqualFold is Rust's eq_ignore_ascii_case: equal under ASCII case
// folding only (strings.EqualFold also folds non-ASCII, e.g. the Kelvin sign).
func asciiEqualFold(a, b string) bool {
	return len(a) == len(b) && asciiLower(a) == asciiLower(b)
}
