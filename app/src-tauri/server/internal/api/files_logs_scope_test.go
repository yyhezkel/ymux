package api

// Files + Logs ops scope proof: every File Manager and Logs huma op checks the
// device's scopes (files:read / files:write / insights:read), so a narrowed
// device is refused while an "all" device and the owner are unaffected.

import (
	"bytes"
	"mime/multipart"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"ymux-server/internal/chat"
	"ymux-server/internal/files"
	"ymux-server/internal/logs"
)

const seedContent = "seeded-secret-content-7f3a"

type scopeOp struct{ method, path, body string }

// Write ops carry bodies valid enough to mutate the sandbox if auth passed.
var (
	filesReadOps = []scopeOp{
		{"GET", "/api/v2/files/list?path=/", ""},
		{"GET", "/api/v2/files/read?path=seed.txt", ""},
		{"GET", "/api/v2/files/download?path=seed.txt", ""},
	}
	filesWriteOps = []scopeOp{
		{"POST", "/api/v2/files/upload?path=up.txt", "upload"},
		{"DELETE", "/api/v2/files/delete?path=seed.txt", ""},
		{"POST", "/api/v2/files/mkdir", `{"path":"newdir"}`},
		{"POST", "/api/v2/files/rename", `{"from":"seed.txt","to":"moved.txt"}`},
		{"POST", "/api/v2/files/copy", `{"from":"seed.txt","to":"copy.txt"}`},
		{"POST", "/api/v2/files/archive", `{"cwd":"/","names":["seed.txt"],"output":"a.zip","format":"zip"}`},
		{"POST", "/api/v2/files/unzip", `{"path":"a.zip"}`},
	}
	// Empty client_id makes the SSE stream end at once instead of tailing.
	logsOps = []scopeOp{
		{"GET", "/api/v2/logs/list", ""},
		{"GET", "/api/v2/logs/read?client_id=server", ""},
		{"GET", "/api/v2/logs/stream", ""},
	}
)

// filesLogsHandler serves files rooted at a seeded temp dir plus an empty log store.
func filesLogsHandler(t *testing.T) (h http.Handler, root string) {
	t.Helper()
	store, err := chat.OpenChatStore(filepath.Join(t.TempDir(), "chat.db"))
	if err != nil {
		t.Fatalf("open store: %v", err)
	}
	t.Cleanup(store.Close)
	c := chat.NewChatAPI(nil, store, termOwner)
	c.SetApprovalAsker(func(string, string, string, map[string]any, int) (string, error) {
		return "allow", nil
	})
	root = t.TempDir()
	if err := os.WriteFile(filepath.Join(root, "seed.txt"), []byte(seedContent), 0o644); err != nil {
		t.Fatalf("seed: %v", err)
	}
	lf, err := files.NewLocalFiles(root, 0)
	if err != nil {
		t.Fatalf("local files: %v", err)
	}
	st, err := logs.NewStore(t.TempDir(), "")
	if err != nil {
		t.Fatalf("log store: %v", err)
	}
	h = NewServer(termOwner, 0, Deps{Chat: c, Files: files.NewService(lf), Logs: logs.NewService(st)}).Handler()
	return h, root
}

// scopeDo sends one op; the "upload" body becomes a multipart `file` part.
func scopeDo(h http.Handler, op scopeOp, token string) *httptest.ResponseRecorder {
	if op.body != "upload" {
		return termDo(h, op.method, op.path, token, op.body)
	}
	var buf bytes.Buffer
	mw := multipart.NewWriter(&buf)
	fw, _ := mw.CreateFormFile("file", "up.txt")
	_, _ = fw.Write([]byte("uploaded"))
	_ = mw.Close()
	r := httptest.NewRequest(op.method, op.path, &buf)
	r.Header.Set("Content-Type", mw.FormDataContentType())
	if token != "" {
		r.Header.Set("Authorization", "Bearer "+token)
	}
	w := httptest.NewRecorder()
	h.ServeHTTP(w, r)
	return w
}

func setScopes(t *testing.T, h http.Handler, id, scopesJSON string) {
	t.Helper()
	pw := termDo(h, "PUT", "/api/v2/devices/"+id+"/scopes", termOwner, `{"scopes":`+scopesJSON+`}`)
	if pw.Code != 200 {
		t.Fatalf("owner set scopes: %d %s", pw.Code, pw.Body.String())
	}
}

func allScopeOps() []scopeOp {
	var all []scopeOp
	all = append(all, filesReadOps...)
	all = append(all, filesWriteOps...)
	return append(all, logsOps...)
}

func TestFilesLogsOpsRefuseNarrowedDevice(t *testing.T) {
	// Pins: a device narrowed to workspace:read gets 403 on all 13 files/logs
	// ops, leaks no file content, and mutates nothing. Breaking it means a
	// scoped-down paired device can still browse or change the server's files.
	h, root := filesLogsHandler(t)
	id, tok := pairDevice(t, h)
	setScopes(t, h, id, `["workspace:read"]`)
	for _, op := range allScopeOps() {
		w := scopeDo(h, op, tok)
		if w.Code != http.StatusForbidden {
			t.Errorf("%s %s narrowed device: want 403 got %d", op.method, op.path, w.Code)
		}
		if strings.Contains(w.Body.String(), seedContent) {
			t.Errorf("%s %s leaked seeded content", op.method, op.path)
		}
	}
	got, err := os.ReadFile(filepath.Join(root, "seed.txt"))
	if err != nil || string(got) != seedContent {
		t.Fatalf("seeded file changed: %q %v", got, err)
	}
}

func TestFilesReadDeviceReadsButCannotWrite(t *testing.T) {
	// Pins: files:read alone lists/reads/downloads but is refused on every
	// write op and on logs (insights:read). Breaking it means read and write
	// grants are no longer separable.
	h, root := filesLogsHandler(t)
	id, tok := pairDevice(t, h)
	setScopes(t, h, id, `["files:read"]`)

	lw := termDo(h, "GET", "/api/v2/files/list?path=/", tok, "")
	if lw.Code != http.StatusOK || !strings.Contains(lw.Body.String(), "seed.txt") {
		t.Errorf("list: %d %s", lw.Code, lw.Body.String())
	}
	rw := termDo(h, "GET", "/api/v2/files/read?path=seed.txt", tok, "")
	if rw.Code != http.StatusOK || !strings.Contains(rw.Body.String(), seedContent) {
		t.Errorf("read: %d %s", rw.Code, rw.Body.String())
	}
	dw := termDo(h, "GET", "/api/v2/files/download?path=seed.txt", tok, "")
	if dw.Code == http.StatusUnauthorized || dw.Code == http.StatusForbidden {
		t.Errorf("download: %d", dw.Code)
	}
	for _, op := range append(append([]scopeOp{}, filesWriteOps...), logsOps...) {
		if w := scopeDo(h, op, tok); w.Code != http.StatusForbidden {
			t.Errorf("%s %s files:read device: want 403 got %d", op.method, op.path, w.Code)
		}
	}
	got, err := os.ReadFile(filepath.Join(root, "seed.txt"))
	if err != nil || string(got) != seedContent {
		t.Fatalf("seeded file changed: %q %v", got, err)
	}
	for _, name := range []string{"up.txt", "newdir", "moved.txt", "copy.txt", "a.zip"} {
		if _, err := os.Stat(filepath.Join(root, name)); err == nil {
			t.Errorf("%s created by refused write op", name)
		}
	}
}

func TestFilesLogsOpsAcceptAllScopeDevice(t *testing.T) {
	// Pins: the default "all" device and the owner pass auth on every op
	// (anything but 401/403). Breaking it means the new scope rows lock out
	// existing un-narrowed devices — the backwards-compat promise.
	h, _ := filesLogsHandler(t)
	_, tok := pairDevice(t, h)
	for _, op := range allScopeOps() {
		for _, who := range []string{termOwner, tok} {
			if w := scopeDo(h, op, who); w.Code == http.StatusUnauthorized || w.Code == http.StatusForbidden {
				t.Errorf("%s %s all-scope token: %d", op.method, op.path, w.Code)
			}
		}
	}
}
