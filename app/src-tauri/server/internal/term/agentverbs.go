package term

// agentverbs.go — agent automation from a browser-created session (Phase
// 103, WEB-DESIGN B5): tree, ui.tree, split, send, send-key, set-pane-title,
// set-pane-annotation, pane.scrollback.
//
// The desktop's dispatch() arms, answering the JSON the CLI already prints.
// Two rules decided for the daemon (Yossi, 2026-10-05):
//   - send / send-key reach only panes of the CALLER'S workspace (or the
//     caller itself). The desktop has no such fence; on the daemon an agent in
//     one project must not type into another.
//   - pane.scrollback stays the desktop's error stub (Rule #1) — tmux could
//     answer it, but reading another pane's screen is a privacy decision, not
//     a port.
//
// split differs from the desktop in one honest way: the desktop only edits
// the layout and lets its frontend start the PTY, but a browser may not be
// connected at all, so the daemon creates the new tmux session itself (same
// workspace, same policy, the source pane's cwd) and puts its pane id in the
// layout. The reply adds pane_id and session — the desktop's makes callers
// diff the tree to find the new pane.
//
// Rule #1: logs carry pane ids and byte COUNTS, never what was typed.

import (
	"encoding/json"
	"errors"

	"ymux-server/internal/agent"
	"ymux-server/internal/core"
)

// scrollbackStub is the desktop's text, character for character.
const scrollbackStub = "pane.scrollback: backend does not buffer PTY content (Absolute Rule #1). " +
	"Workaround for tmux: rpc `send` with data `\\u001btmux capture-pane -p -S -<N>\\n` then read the next pty:data event."

// sendMax bounds one send: an agent typing a paste, not a file.
const sendMax = 64 * 1024

type agentParams struct {
	PaneID      string `json:"pane_id"`
	Pane        string `json:"pane"`
	WorkspaceID string `json:"workspace_id"`
	Direction   string `json:"direction"`
	Kind        string `json:"kind"`
	Data        string `json:"data"`
	Key         string `json:"key"`
	Keys        string `json:"keys"`
	Title       string `json:"title"`
	Annotation  string `json:"annotation"`
}

func (p agentParams) pane() string {
	if p.PaneID != "" {
		return p.PaneID
	}
	return p.Pane
}

// byPane finds a hook-enabled session by its pane id.
func (r *HookRegistry) byPane(paneID string) (hookEntry, bool) {
	r.mu.Lock()
	defer r.mu.Unlock()
	for _, e := range r.byName {
		if e.paneID == paneID {
			return *e, true
		}
	}
	return hookEntry{}, false
}

// agentVerb runs one B5 method. ok is false when method is not one.
func (t termHookTarget) agentVerb(method string, raw json.RawMessage) (any, *core.RPCError, bool) {
	switch method {
	case "tree", "ui.tree", "split", "action.split", "send", "send-key", "action.send_keys",
		"set-pane-title", "set-pane-annotation", "pane.scrollback":
	default:
		return nil, nil, false
	}
	if method == "pane.scrollback" {
		return nil, rpcErr(scrollbackStub), true
	}
	var p agentParams
	if len(raw) > 0 && json.Unmarshal(raw, &p) != nil {
		return nil, rpcErr("bad params"), true
	}
	r := t.r
	r.mu.Lock()
	self := *t.e
	r.mu.Unlock()

	switch method {
	case "tree":
		id := p.WorkspaceID
		if id == "" {
			id = self.workspaceID
		}
		ws, ok := r.webws.get(id)
		if !ok {
			return nil, nil, true // the desktop answers null when nothing matches
		}
		layout := json.RawMessage("null")
		if len(ws.Layout) > 0 {
			layout = ws.Layout
		}
		return map[string]any{"workspace_id": ws.ID, "name": ws.Name, "layout": layout}, nil, true

	case "ui.tree":
		var out []map[string]any
		for _, ws := range r.webws.list() {
			root, _ := parseLayout(ws.Layout)
			panes := []map[string]any{}
			for _, l := range paneIDs(root) {
				kind, _ := l["pane_kind"].(string)
				if kind == "" {
					kind = "terminal"
				}
				panes = append(panes, map[string]any{"pane_id": l["pane_id"], "kind": kind,
					"title": l["title"], "annotation": l["annotation"]})
			}
			out = append(out, map[string]any{"workspace_id": ws.ID, "name": ws.Name,
				"is_active": ws.ID == self.workspaceID, "panes": panes})
		}
		if out == nil {
			out = []map[string]any{}
		}
		var active any
		if self.workspaceID != "" {
			active = self.workspaceID
		}
		return map[string]any{"active_workspace_id": active, "workspaces": out}, nil, true

	case "split", "action.split":
		return t.split(p, self)

	case "send", "send-key", "action.send_keys":
		target, rerr := t.scoped(p.pane(), self)
		if rerr != nil {
			return nil, rerr, true
		}
		var b []byte
		if method == "send" {
			b = []byte(p.Data)
		} else {
			key := p.Key
			if key == "" {
				key = p.Keys
			}
			if key == "" {
				return nil, rpcErr("missing key"), true
			}
			b = agent.TranslateKey(key)
		}
		if len(b) > sendMax {
			return nil, rpcErr("data too large"), true
		}
		if err := r.tmux.SendBytes(target.name, b); err != nil {
			logger.Warn("send failed", "from", self.paneID, "to", target.paneID, "err", err)
			return nil, rpcErr("pane " + target.paneID + " not connected"), true
		}
		logger.Info("agent send", "from", self.paneID, "to", target.paneID, "bytes", len(b))
		return map[string]any{"ok": true, "bytes": len(b)}, nil, true

	case "set-pane-title", "set-pane-annotation":
		target, rerr := t.scoped(p.pane(), self)
		if rerr != nil {
			return nil, rerr, true
		}
		field, value := "title", p.Title
		if method == "set-pane-annotation" {
			field, value = "annotation", p.Annotation
		}
		ws, err := r.webws.mutateLayout(target.workspaceID, func(root node) (node, error) {
			l := findLeaf(root, target.paneID)
			if l == nil {
				return nil, errNoPane
			}
			if value == "" {
				delete(l, field) // the desktop clears to None
			} else {
				l[field] = value
			}
			return root, nil
		})
		if err != nil {
			return nil, rpcErr(layoutErr(err, target.paneID)), true
		}
		r.workspacesChanged(ws.ID, ws.Version)
		return map[string]any{"ok": true}, nil, true
	}
	return nil, nil, false
}

// scoped resolves a target pane an agent may act on: itself, or a pane of
// its own workspace. Errors use the desktop's wording where one exists.
func (t termHookTarget) scoped(paneID string, self hookEntry) (hookEntry, *core.RPCError) {
	if paneID == "" || paneID == self.paneID {
		return self, nil
	}
	target, ok := t.r.byPane(paneID)
	if !ok {
		return hookEntry{}, rpcErr("pane " + paneID + " not connected")
	}
	if self.workspaceID == "" || target.workspaceID != self.workspaceID {
		logger.Warn("agent verb across workspaces — refused", "from", self.paneID, "to", paneID)
		return hookEntry{}, rpcErr("pane " + paneID + " is not in this session's workspace")
	}
	return target, nil
}

// split creates a new tmux session in the source pane's workspace and splits
// the source leaf to hold it.
func (t termHookTarget) split(p agentParams, self hookEntry) (any, *core.RPCError, bool) {
	r := t.r
	switch p.Kind {
	case "", "terminal":
	case "browser":
		return nil, rpcErr("bad kind: browser (no in-app browser in browser mode)"), true
	default:
		return nil, rpcErr("bad kind: " + p.Kind), true
	}
	dir, ok := splitDirection(p.Direction)
	if !ok {
		return nil, rpcErr("bad direction: " + p.Direction), true
	}
	src, rerr := t.scoped(p.pane(), self)
	if rerr != nil {
		return nil, rerr, true
	}
	if src.workspaceID == "" || !r.webws.exists(src.workspaceID) {
		return nil, rpcErr("no pane " + src.paneID + " in a workspace"), true
	}
	if r.spawn == nil {
		return nil, rpcErr("split unavailable"), true
	}
	created, err := r.spawn("", r.tmux.CurrentPath(src.name), src.policy, src.workspaceID)
	if err != nil {
		logger.Error("split: create failed", "from", src.paneID, "err", err)
		return nil, rpcErr("split: could not create a session"), true
	}
	ws, err := r.webws.mutateLayout(src.workspaceID, func(root node) (node, error) {
		if root == nil {
			root = leaf(src.paneID) // a workspace whose layout was never written
		}
		return splitLeaf(root, src.paneID, created.paneID, dir)
	})
	if err != nil {
		// Do not leave a session the layout does not show.
		_ = r.tmux.Kill(created.name)
		r.Remove(created.name)
		return nil, rpcErr("split: " + layoutErr(err, src.paneID)), true
	}
	r.workspacesChanged(ws.ID, ws.Version)
	logger.Info("agent split", "from", src.paneID, "new", created.paneID, "direction", dir)
	return map[string]any{"ok": true, "workspace_id": ws.ID, "split_from": src.paneID,
		"pane_id": created.paneID, "session": created.name}, nil, true
}

func layoutErr(err error, paneID string) string {
	switch {
	case errors.Is(err, errNoPane):
		return "no pane " + paneID
	case errors.Is(err, errNoWorkspace):
		return "no such workspace"
	}
	logger.Error("layout update failed", "pane", paneID, "err", err)
	return "layout save failed"
}
