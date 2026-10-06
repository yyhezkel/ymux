package term

// huma.go — the four terminal REST ops (list, create, rename, kill) as typed
// huma operations, so they land in the generated OpenAPI and the SDK drift
// check covers them. Auth is NOT here: api's bearerMiddleware guards them via
// opScopes (shell:attach). Attach and the rest of /api/v2/term/* stay raw
// behind gate — a WebSocket and the page's own routes are not OpenAPI shapes.

import (
	"context"
	"errors"
	"net/http"
	"strings"

	"github.com/danielgtaylor/huma/v2"
)

var secured = []map[string][]string{{"bearerAuth": {}}}

// TermCreateRequest is the create body. Optional and open: the raw handler
// ignored decode errors and unknown fields (the browser client also sends
// pane_id/cmd), and huma must not start answering 422 for either.
type TermCreateRequest struct {
	_           struct{} `json:"-" additionalProperties:"true"`
	Name        string   `json:"name,omitempty"`
	Cwd         string   `json:"cwd,omitempty"`
	Policy      string   `json:"policy,omitempty"`       // "none" (default) | "gate"
	WorkspaceID string   `json:"workspace_id,omitempty"` // a browser workspace
	PaneID      string   `json:"pane_id,omitempty"`      // Phase 109: the browser leaf this session fills
	// Phase 110: an argv the session runs instead of a shell — a browser pane
	// opened in "claude" mode. argv, never a shell string (Rule #3).
	Cmd []string `json:"cmd,omitempty"`
}

// TermCreated is the create response; same keys the raw handler's map had.
type TermCreated struct {
	Name        string `json:"name"`
	Display     string `json:"display"`
	Hooks       bool   `json:"hooks"`
	Policy      string `json:"policy,omitempty"`
	PaneID      string `json:"pane_id,omitempty"`
	WorkspaceID string `json:"workspace_id,omitempty"`
}

// TermRenameRequest is the rename body (optional, open — see TermCreateRequest).
type TermRenameRequest struct {
	_       struct{} `json:"-" additionalProperties:"true"`
	NewName string   `json:"new_name,omitempty"`
}

// TermRenamed is the rename response.
type TermRenamed struct {
	Name string `json:"name"`
}

// TermKilled is the kill response.
type TermKilled struct {
	OK bool `json:"ok"`
}

// humaErr maps a package error to a huma status error. Same table as failErr.
func humaErr(err error) error {
	switch {
	case errors.Is(err, ErrNoSession):
		return huma.NewError(http.StatusNotFound, err.Error())
	case errors.Is(err, ErrBadName):
		return huma.NewError(http.StatusBadRequest, err.Error())
	case errors.Is(err, errSessionExists), errors.Is(err, errPaneInUse):
		return huma.NewError(http.StatusConflict, err.Error())
	case errors.Is(err, errBadPaneID):
		return huma.NewError(http.StatusBadRequest, err.Error())
	default:
		logger.Error("tmux command failed", "err", err)
		return huma.NewError(http.StatusInternalServerError, "tmux command failed")
	}
}

// RegisterHuma mounts the four terminal operations onto a shared huma API.
func (s *Service) RegisterHuma(api huma.API) {
	huma.Register(api, huma.Operation{
		OperationID: "term-list", Method: http.MethodGet, Path: "/api/v2/term/sessions",
		Summary: "List terminal (tmux) sessions",
		Tags:    []string{"term"}, Security: secured,
	}, func(_ context.Context, _ *struct{}) (*struct{ Body []Annotated }, error) {
		sessions, err := s.tmux.List()
		if err != nil {
			return nil, humaErr(err)
		}
		if s.hooks != nil {
			s.hooks.Retain(sessions)
		}
		meta := LoadMeta(s.home)
		// Counts only — a session NAME can carry a branch or a client name, and a
		// meta entry carries user-written labels (Rule #1).
		logger.Info("terminal sessions listed", "sessions", len(sessions), "labelled", len(meta))
		return &struct{ Body []Annotated }{Body: Annotate(sessions, meta)}, nil
	})

	huma.Register(api, huma.Operation{
		OperationID: "term-create", Method: http.MethodPost, Path: "/api/v2/term/sessions",
		Summary:       "Create a terminal session",
		DefaultStatus: http.StatusCreated,
		Tags:          []string{"term"}, Security: secured,
	}, func(_ context.Context, in *struct {
		Body TermCreateRequest `required:"false"`
	}) (*struct{ Body TermCreated }, error) {
		body := in.Body
		if body.Policy == "" {
			body.Policy = policyNone
		}
		if !validPolicy(body.Policy) {
			return nil, huma.NewError(http.StatusBadRequest, `policy must be "none" or "gate"`)
		}
		if body.WorkspaceID != "" && (s.hooks == nil || !s.hooks.webws.exists(body.WorkspaceID)) {
			return nil, huma.NewError(http.StatusBadRequest, "no such workspace")
		}
		cmd, err := s.sessionArgv(body.Cmd)
		if err != nil {
			return nil, huma.NewError(http.StatusBadRequest, err.Error())
		}
		e, hooks, err := s.spawnSession(body.Name, body.Cwd, body.Policy, body.WorkspaceID, body.PaneID, cmd...)
		if err != nil {
			return nil, humaErr(err)
		}
		out := TermCreated{Name: e.name, Display: e.name, Hooks: hooks}
		if hooks {
			out.Policy, out.PaneID, out.WorkspaceID = e.policy, e.paneID, e.workspaceID
		}
		return &struct{ Body TermCreated }{Body: out}, nil
	})

	huma.Register(api, huma.Operation{
		OperationID: "term-rename", Method: http.MethodPost, Path: "/api/v2/term/sessions/{name}/rename",
		Summary: "Rename a terminal session",
		Tags:    []string{"term"}, Security: secured,
	}, func(_ context.Context, in *struct {
		Name string            `path:"name"`
		Body TermRenameRequest `required:"false"`
	}) (*struct{ Body TermRenamed }, error) {
		to := strings.TrimSpace(in.Body.NewName)
		from := in.Name
		if !ValidName(to) {
			return nil, humaErr(ErrBadName)
		}
		if s.tmux.Has(to) && to != from {
			return nil, humaErr(errSessionExists)
		}
		if err := s.tmux.Rename(from, to); err != nil {
			return nil, humaErr(err)
		}
		if s.hooks != nil {
			s.hooks.Rename(from, to)
		}
		// The session-meta entry is keyed by NAME, so a rename orphans it. The
		// CLI's hooks re-key it on the session's next turn and its pruning drops
		// the stale key, so this is self-healing rather than something to patch up
		// from here — and writing that file from the daemon would put a second
		// writer on it (the CLI's atomic tmp+rename assumes one).
		return &struct{ Body TermRenamed }{Body: TermRenamed{Name: to}}, nil
	})

	huma.Register(api, huma.Operation{
		OperationID: "term-kill", Method: http.MethodDelete, Path: "/api/v2/term/sessions/{name}",
		Summary: "Kill a terminal session",
		Tags:    []string{"term"}, Security: secured,
	}, func(_ context.Context, in *struct {
		Name string `path:"name"`
	}) (*struct{ Body TermKilled }, error) {
		if err := s.tmux.Kill(in.Name); err != nil {
			return nil, humaErr(err)
		}
		if s.hooks != nil {
			s.hooks.Remove(in.Name)
		}
		return &struct{ Body TermKilled }{Body: TermKilled{OK: true}}, nil
	})
}
