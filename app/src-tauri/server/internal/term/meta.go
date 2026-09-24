package term

// meta.go — the read side of ~/.ymux/session-meta.json, the file the Linux CLI
// writes (cli/src/session_meta.rs) so every ymux desktop labels the same tmux
// sessions identically. The browser reads it through this package for the same
// reason; nothing here writes it, so there is no second writer to race with
// the CLI's atomic tmp+rename.
//
// Rule #1: label / auto_name / claude_title are user content. They are joined
// into an API response by design (that IS the feature) and are never logged.

import (
	"encoding/json"
	"os"
	"path/filepath"
)

// MetaEntry mirrors the CLI's SessionMetaEntry. Every field is optional — the
// file is written incrementally by different hooks, and a half-populated entry
// is normal, not corrupt.
type MetaEntry struct {
	ClaudeSessionID string `json:"claude_session_id,omitempty"`
	ClaudeTitle     string `json:"claude_title,omitempty"`
	AutoName        string `json:"auto_name,omitempty"`
	Label           string `json:"label,omitempty"`
	Origin          string `json:"origin,omitempty"`
	UpdatedAt       string `json:"updated_at,omitempty"`
}

// metaFile is the on-disk document.
type metaFile struct {
	Version  int                  `json:"version"`
	Sessions map[string]MetaEntry `json:"sessions"`
}

// MetaPath is the file's location under a home directory.
func MetaPath(home string) string {
	return filepath.Join(home, ".ymux", "session-meta.json")
}

// LoadMeta reads the session-meta map. Missing, unreadable and corrupt all
// degrade to an empty map, exactly as the CLI's loader does: a session list
// with no labels is far better than a 500, and the CLI rebuilds the file on
// its next write.
func LoadMeta(home string) map[string]MetaEntry {
	b, err := os.ReadFile(MetaPath(home))
	if err != nil {
		return map[string]MetaEntry{}
	}
	var f metaFile
	if json.Unmarshal(b, &f) != nil || f.Sessions == nil {
		return map[string]MetaEntry{}
	}
	return f.Sessions
}

// DisplayName applies the precedence documented in docs/ARCHITECTURE.md:
// label > auto_name > claude_title > the raw tmux name.
//
// The ordering is not arbitrary. claude_title churns — Claude rewrites its own
// summary every turn — so it is the weakest signal; auto_name is derived once
// from the session's first real prompt and is therefore the stable identity;
// label is what a human typed and always wins.
func DisplayName(rawName string, e MetaEntry) string {
	switch {
	case e.Label != "":
		return e.Label
	case e.AutoName != "":
		return e.AutoName
	case e.ClaudeTitle != "":
		return e.ClaudeTitle
	default:
		return rawName
	}
}

// Annotated is a tmux session joined with its session-meta entry — the shape
// the API returns.
type Annotated struct {
	Session
	Display         string `json:"display"`
	Label           string `json:"label,omitempty"`
	AutoName        string `json:"auto_name,omitempty"`
	ClaudeTitle     string `json:"claude_title,omitempty"`
	ClaudeSessionID string `json:"claude_session_id,omitempty"`
	Origin          string `json:"origin,omitempty"`
}

// Annotate joins the live tmux list with the meta map. Sessions with no meta
// entry come back with Display set to the raw name, never dropped — tmux is
// the truth, and the meta file is decoration.
func Annotate(sessions []Session, meta map[string]MetaEntry) []Annotated {
	out := make([]Annotated, 0, len(sessions))
	for _, s := range sessions {
		e := meta[s.Name]
		out = append(out, Annotated{
			Session:         s,
			Display:         DisplayName(s.Name, e),
			Label:           e.Label,
			AutoName:        e.AutoName,
			ClaudeTitle:     e.ClaudeTitle,
			ClaudeSessionID: e.ClaudeSessionID,
			Origin:          e.Origin,
		})
	}
	return out
}
