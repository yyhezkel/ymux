package term

// layout.go — the few layout-tree operations the daemon itself performs
// (Phase 103, WEB-DESIGN B5).
//
// The layout is the desktop's LayoutNode JSON (crates/ymux-types):
//
//	{"kind":"pane","pane_id":…, …optional title/annotation/…}
//	{"kind":"split","split_id":…,"direction":"horizontal"|"vertical",
//	 "first":{…},"second":{…},"ratio":0.5}
//
// It is stored OPAQUE — the browser owns the tree and PUTs it (WEB-DESIGN
// §4) — except for what an agent can ask of the daemon over the hook RPC:
// "is this pane here", "split leaf X", "set a leaf's title / annotation".
// Nothing else is ported (§6: "do not port the whole op set to Go").
//
// The tree is handled as generic JSON maps on purpose: every field the daemon
// does not know (connection, color, emoji, browser state, a field the desktop
// adds next year) round-trips byte-for-byte in meaning, instead of being
// dropped by a Go struct that is one release behind.

import (
	"encoding/json"
	"errors"
	"fmt"
	"sync/atomic"
	"time"
)

var errNoPane = errors.New("pane not in layout")

type node = map[string]any

func parseLayout(raw json.RawMessage) (node, error) {
	if len(raw) == 0 || string(raw) == "null" {
		return nil, nil
	}
	var n node
	if err := json.Unmarshal(raw, &n); err != nil {
		return nil, err
	}
	return n, nil
}

func leaf(paneID string) node { return node{"kind": "pane", "pane_id": paneID} }

// findLeaf returns the pane node with paneID, or nil.
func findLeaf(n node, paneID string) node {
	if n == nil {
		return nil
	}
	switch n["kind"] {
	case "pane":
		if n["pane_id"] == paneID {
			return n
		}
	case "split":
		first, _ := n["first"].(map[string]any)
		if f := findLeaf(first, paneID); f != nil {
			return f
		}
		second, _ := n["second"].(map[string]any)
		return findLeaf(second, paneID)
	}
	return nil
}

var layoutSeq atomic.Uint64

// newSplitID mirrors the desktop's `sp_{nanos:x}_{ctr:x}`.
func newSplitID() string {
	return fmt.Sprintf("sp_%x_%x", time.Now().UnixNano(), layoutSeq.Add(1)-1)
}

// splitLeaf replaces the leaf paneID with a split of {that leaf, a new leaf
// newPane}, ratio 0.5, as the desktop's split_pane_in does. Returns the new
// root (the root itself changes when the tree was a single leaf).
func splitLeaf(root node, paneID, newPane, direction string) (node, error) {
	var walk func(n node) (node, bool)
	walk = func(n node) (node, bool) {
		if n == nil {
			return nil, false
		}
		switch n["kind"] {
		case "pane":
			if n["pane_id"] == paneID {
				return node{"kind": "split", "split_id": newSplitID(), "direction": direction,
					"first": n, "second": leaf(newPane), "ratio": 0.5}, true
			}
		case "split":
			for _, side := range []string{"first", "second"} {
				child, _ := n[side].(map[string]any)
				if repl, ok := walk(child); ok {
					n[side] = repl
					return n, true
				}
			}
		}
		return n, false
	}
	out, ok := walk(root)
	if !ok {
		return root, errNoPane
	}
	return out, nil
}

// splitDirection normalizes the desktop's accepted spellings.
func splitDirection(d string) (string, bool) {
	switch d {
	case "", "horizontal", "right", "h":
		return "horizontal", true
	case "vertical", "down", "v":
		return "vertical", true
	}
	return "", false
}

// paneIDs lists the leaves depth-first, as ui.tree does.
func paneIDs(n node) []node {
	if n == nil {
		return nil
	}
	if n["kind"] == "pane" {
		return []node{n}
	}
	first, _ := n["first"].(map[string]any)
	second, _ := n["second"].(map[string]any)
	return append(paneIDs(first), paneIDs(second)...)
}
