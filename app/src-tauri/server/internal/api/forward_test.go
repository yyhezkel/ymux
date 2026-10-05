package api

import (
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"testing"

	"ymux-server/internal/insights"
	"ymux-server/internal/workspace"
)

// Phase 101: the CLI forwards every pre-tool-use to /api/v2/hooks/forward,
// including those from a browser-created session whose hook RPC already
// reached the daemon. Those (pane id term_…) are acknowledged and dropped;
// a desktop pane's still becomes a pending request a phone can answer.
func TestHooksForwardDropsBrowserSessions(t *testing.T) {
	st, err := workspace.OpenStore(filepath.Join(t.TempDir(), "ws.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(st.Close)
	mgr := workspace.NewManager(st, nil)
	h := NewServer("secret", 0, Deps{
		Insights:  insights.NewService(nil, nil, ""),
		Workspace: workspace.NewService(mgr, "secret"),
	}).Handler()

	forward := func(reqID, pane string) int {
		body := `{"req_id":"` + reqID + `","workspace_id":"ws1","pane_id":"` + pane + `","tool_name":"Bash","title":"t","timeout_at":0}`
		r := httptest.NewRequest("POST", "/api/v2/hooks/forward", strings.NewReader(body))
		r.Header.Set("Authorization", "Bearer secret")
		r.Header.Set("Content-Type", "application/json")
		w := httptest.NewRecorder()
		h.ServeHTTP(w, r)
		return w.Code
	}

	if code := forward("req-term", "term_0123456789abcdef"); code != http.StatusOK {
		t.Fatalf("browser-session forward → %d, want 200", code)
	}
	if _, ok := st.GetPending("req-term"); ok {
		t.Error("a term_ pane's forwarded hook must not become a pending request")
	}

	if code := forward("req-desk", "p_18ce7ffc3e4a80e8_a"); code != http.StatusOK {
		t.Fatalf("desktop-pane forward → %d, want 200", code)
	}
	if _, ok := st.GetPending("req-desk"); !ok {
		t.Error("a desktop pane's forwarded hook must still become a pending request")
	}
}
