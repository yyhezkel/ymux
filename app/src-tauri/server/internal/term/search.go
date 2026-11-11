package term

// search.go — the one matcher behind the session-history search (Orca idea
// #9): GET /api/v2/claude/sessions?q= and GET /api/v2/term/history?q=.
//
// Semantics are the same as matchesQuery in app/src/paletteMatch.ts, which
// the desktop resume picker uses. testdata/search_match.json pins both:
// search_test.go and paletteMatch.test.ts read the same cases.
//
// Rule #1: a query is user text. Callers log counts and timing only.

import (
	"net/url"
	"strings"
)

// matchTokens is true when every whitespace-separated token of q is a
// case-insensitive substring of at least one field. A token can match a
// different field than its neighbours. An empty query matches everything.
func matchTokens(q string, fields ...string) bool {
	tokens := strings.Fields(strings.ToLower(q))
	if len(tokens) == 0 {
		return true
	}
	hay := make([]string, len(fields))
	for i, f := range fields {
		hay[i] = strings.ToLower(f)
	}
	for _, t := range tokens {
		found := false
		for _, h := range hay {
			if strings.Contains(h, t) {
				found = true
				break
			}
		}
		if !found {
			return false
		}
	}
	return true
}

// containsFold is one field filter: an empty want passes, else want must be
// a case-insensitive substring of have.
func containsFold(have, want string) bool {
	return want == "" || strings.Contains(strings.ToLower(have), strings.ToLower(want))
}

// sessionQuery is the search part of a request: the free text plus the
// optional field filters. The zero value matches everything.
type sessionQuery struct {
	Q, Branch, Model string
}

// searchMaxRunes caps each query value. A longer value is cut, not refused:
// a pasted line still searches.
const searchMaxRunes = 200

func parseSessionQuery(v url.Values) sessionQuery {
	cut := func(s string) string {
		s = strings.TrimSpace(s)
		if r := []rune(s); len(r) > searchMaxRunes {
			s = string(r[:searchMaxRunes])
		}
		return s
	}
	return sessionQuery{Q: cut(v.Get("q")), Branch: cut(v.Get("branch")), Model: cut(v.Get("model"))}
}

// active is true when the request asks for any filtering.
func (q sessionQuery) active() bool {
	return strings.TrimSpace(q.Q) != "" || q.Branch != "" || q.Model != ""
}

// match applies the field filters, then the token-AND text over the
// searchable fields: title, cwd, branch, model, preview.
func (q sessionQuery) match(title, cwd, branch, model string, preview ...string) bool {
	if !containsFold(branch, q.Branch) || !containsFold(model, q.Model) {
		return false
	}
	return matchTokens(q.Q, append([]string{title, cwd, branch, model}, preview...)...)
}
