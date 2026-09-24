package auth

// scopes.go — per-device authorization grants (Phase 77 §Q6). A paired device
// carries a set of scopes (stored as a JSON array string on the device row). A
// missing/empty value or the sentinel "all" means "every grant" — the default
// on redeem, so existing devices keep full access (backwards compat) until an
// owner narrows them.

import (
	"encoding/json"
	"strings"
)

// Scope is a single capability grant.
type Scope string

const (
	ScopeWorkspaceRead  Scope = "workspace:read"
	ScopeWorkspaceWrite Scope = "workspace:write"
	ScopeSessionRead    Scope = "session:read"
	ScopeSessionWrite   Scope = "session:write"
	ScopeHookApprove    Scope = "hook:approve"
	ScopeHookDeny       Scope = "hook:deny"
	ScopeFilesRead      Scope = "files:read"
	ScopeFilesWrite     Scope = "files:write"
	ScopeInsightsRead   Scope = "insights:read"

	// ScopeShellAttach grants a real terminal on this machine — listing,
	// creating, renaming and killing tmux sessions, and attaching a PTY to one
	// (internal/term). It is DELIBERATELY NOT in AllScopes: ParseScopes fails
	// open to AllScopes for "", "all" and anything malformed, so every scope in
	// that list is reachable by a device that was never explicitly restricted.
	// A leaked token must not become a shell over the internet, so this one is
	// only ever held by a device whose grants name it explicitly. It is valid
	// (grantable + storable), just never implied.
	ScopeShellAttach Scope = "shell:attach"
)

// AllScopes is every grant a device holds when it has NOT been explicitly
// restricted — i.e. the fail-open set. ScopeShellAttach is not a member; see
// its doc comment.
var AllScopes = []Scope{
	ScopeWorkspaceRead, ScopeWorkspaceWrite,
	ScopeSessionRead, ScopeSessionWrite,
	ScopeHookApprove, ScopeHookDeny,
	ScopeFilesRead, ScopeFilesWrite,
	ScopeInsightsRead,
}

// GrantableScopes is every scope an owner may grant: AllScopes plus the
// opt-in-only ones. ValidScope reads this; ParseScopes' fail-open default
// reads AllScopes. Keeping the two lists separate is what makes
// "explicitly granted" different from "not restricted".
var GrantableScopes = append(append([]Scope{}, AllScopes...), ScopeShellAttach)

// ValidScope reports whether s is a grantable scope.
func ValidScope(s string) bool {
	for _, k := range GrantableScopes {
		if Scope(s) == k {
			return true
		}
	}
	return false
}

// ParseScopes reads the stored scopes string into a grant list. "", "all", or
// a malformed value ⇒ AllScopes (fail-open for backwards compat — a device is
// only restricted by an explicit, well-formed subset).
func ParseScopes(stored string) []Scope {
	t := strings.TrimSpace(stored)
	if t == "" || t == "all" || t == `"all"` {
		return AllScopes
	}
	var arr []string
	if err := json.Unmarshal([]byte(t), &arr); err != nil {
		return AllScopes
	}
	out := make([]Scope, 0, len(arr))
	for _, x := range arr {
		out = append(out, Scope(x))
	}
	return out
}

// HasScope reports whether the stored scopes grant want.
func HasScope(stored string, want Scope) bool {
	for _, s := range ParseScopes(stored) {
		if s == want {
			return true
		}
	}
	return false
}

// NormalizeScopes validates + serializes a requested grant list to the stored
// JSON form. Unknown scopes are dropped. An empty result stores "all".
// Returns the canonical stored string.
//
// A list that covers exactly AllScopes also collapses to "all" — the two mean
// the same thing. A list carrying an opt-in-only scope (shell:attach) never
// collapses, because "all" does NOT imply it: it is always stored explicitly.
func NormalizeScopes(req []string) string {
	seen := map[string]bool{}
	var keep []string
	optIn := false
	for _, s := range req {
		if !ValidScope(s) || seen[s] {
			continue
		}
		seen[s] = true
		keep = append(keep, s)
		if !inAllScopes(s) {
			optIn = true
		}
	}
	if len(keep) == 0 {
		return "all"
	}
	if !optIn && len(keep) == len(AllScopes) {
		return "all"
	}
	b, _ := json.Marshal(keep)
	return string(b)
}

// inAllScopes reports whether s is part of the fail-open set.
func inAllScopes(s string) bool {
	for _, k := range AllScopes {
		if Scope(s) == k {
			return true
		}
	}
	return false
}
