package api

import (
	"encoding/json"
	"strings"
	"testing"

	"ymux-server/internal/files"
	"ymux-server/internal/logs"
	"ymux-server/internal/term"
)

// TestOpenAPISpecComplete is a unit-level drift guard: the generated spec must
// cover the whole client-SDK surface (Files + Logs + meta) with the bearer
// scheme declared. If someone adds/renames an op without it showing up here,
// this fails before the SDK pipeline's diff-based CI guard would.
func TestOpenAPISpecComplete(t *testing.T) {
	// nil providers are fine: registration only reflects types, handlers unused.
	s := NewServer("secret", 0, Deps{
		Files: files.NewService(nil),
		Logs:  logs.NewService(nil),
		Term:  term.NewService("", ""),
	})
	raw, err := s.OpenAPISpec()
	if err != nil {
		t.Fatalf("OpenAPISpec: %v", err)
	}
	var doc struct {
		OpenAPI    string                            `json:"openapi"`
		Paths      map[string]map[string]any         `json:"paths"`
		Components struct {
			SecuritySchemes map[string]any `json:"securitySchemes"`
		} `json:"components"`
	}
	if err := json.Unmarshal(raw, &doc); err != nil {
		t.Fatalf("spec is not valid JSON: %v", err)
	}
	if doc.OpenAPI == "" {
		t.Fatal("missing openapi version")
	}
	if _, ok := doc.Components.SecuritySchemes["bearerAuth"]; !ok {
		t.Fatal("bearerAuth security scheme missing")
	}
	want := []string{
		"/healthz", "/api/version",
		"/api/v2/files/list", "/api/v2/files/read", "/api/v2/files/upload",
		"/api/v2/files/download", "/api/v2/files/delete",
		"/api/v2/logs/list", "/api/v2/logs/read", "/api/v2/logs/stream",
		"/api/v2/term/sessions", "/api/v2/term/sessions/{name}",
		"/api/v2/term/sessions/{name}/rename",
	}
	for _, p := range want {
		if _, ok := doc.Paths[p]; !ok {
			t.Errorf("generated spec missing path %s", p)
		}
	}
	// The attach WebSocket is a raw route; it must never leak into the spec.
	for p := range doc.Paths {
		if strings.Contains(p, "/attach") {
			t.Errorf("spec exposes %s; attach is WS-only", p)
		}
	}
}
