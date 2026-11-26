package insights

// Grok Build writes a running token total on each turn_completed in
// ~/.grok/sessions/<group>/<id>/updates.jsonl. Counting every line would
// multiply the spend, so each line contributes only the increase since
// the previous line in that file. A dip does not rewind the baseline.
// Tokens only — prices stay in app/src/claudePricing.ts. Message text
// is not read.

import (
	"bufio"
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
)

func grokSessionsDir() string {
	home, err := os.UserHomeDir()
	if err != nil {
		return ""
	}
	return filepath.Join(home, ".grok", "sessions")
}

// grokUsageDir is folded into scanClaudeUsage. Empty skips the walk.
// TestMain clears it so a developer's real ~/.grok cannot change the
// Claude-only counts. Production leaves the default.
var grokUsageDir = grokSessionsDir()

type grokNums struct {
	in, out, cache int64
}

func grokUsageNums(raw json.RawMessage) grokNums {
	var u struct {
		InputTokens      int64 `json:"inputTokens"`
		OutputTokens     int64 `json:"outputTokens"`
		CachedReadTokens int64 `json:"cachedReadTokens"`
	}
	if json.Unmarshal(raw, &u) != nil {
		return grokNums{}
	}
	in := u.InputTokens - u.CachedReadTokens
	if in < 0 {
		in = 0
	}
	return grokNums{in: in, out: u.OutputTokens, cache: u.CachedReadTokens}
}

// grokDelta is the increase since prev. next is the higher of the two,
// so a smaller reading does not become the new baseline.
func grokDelta(cur, prev grokNums) (delta, next grokNums) {
	clamp := func(a, b int64) int64 {
		if a < b {
			return 0
		}
		return a - b
	}
	delta = grokNums{in: clamp(cur.in, prev.in), out: clamp(cur.out, prev.out), cache: clamp(cur.cache, prev.cache)}
	next = grokNums{in: max(cur.in, prev.in), out: max(cur.out, prev.out), cache: max(cur.cache, prev.cache)}
	return delta, next
}

func scanGrokUsage(
	root string, since, until int64, rep *ClaudeUsageReport,
	buckets map[int64]*ClaudeTokens, byModel, byProject, bySession *claudeAgg,
) {
	if root == "" {
		return
	}
	groups, err := os.ReadDir(root)
	if err != nil {
		return
	}
	for _, group := range groups {
		if !group.IsDir() {
			continue
		}
		sessions, err := os.ReadDir(filepath.Join(root, group.Name()))
		if err != nil {
			continue
		}
		for _, session := range sessions {
			if !session.IsDir() {
				continue
			}
			dir := filepath.Join(root, group.Name(), session.Name())
			updates := filepath.Join(dir, "updates.jsonl")
			info, err := os.Stat(updates)
			if err != nil || !info.Mode().IsRegular() {
				continue
			}
			if info.ModTime().Unix() < since {
				rep.SkippedFiles++
				continue
			}
			rep.ScannedFiles++
			cwd := readGrokCwd(filepath.Join(dir, "summary.json"))
			scanGrokFile(updates, since, until, cwd, session.Name(), rep, buckets, byModel, byProject, bySession)
		}
	}
}

func readGrokCwd(path string) string {
	var raw struct {
		Info struct {
			Cwd string `json:"cwd"`
		} `json:"info"`
	}
	b, err := os.ReadFile(path)
	if err != nil || json.Unmarshal(b, &raw) != nil {
		return ""
	}
	return raw.Info.Cwd
}

func scanGrokFile(
	path string, since, until int64, cwd, sid string, rep *ClaudeUsageReport,
	buckets map[int64]*ClaudeTokens, byModel, byProject, bySession *claudeAgg,
) {
	fh, err := os.Open(path)
	if err != nil {
		rep.SkippedFiles++
		return
	}
	defer fh.Close()
	prevModels := map[string]grokNums{}
	var prevTop grokNums
	sc := bufio.NewScanner(fh)
	sc.Buffer(make([]byte, 64*1024), 16<<20)
	for sc.Scan() {
		line := sc.Bytes()
		if !bytes.Contains(line, []byte("turn_completed")) {
			continue
		}
		var ev struct {
			Params struct {
				Meta struct {
					AgentTimestampMs int64  `json:"agentTimestampMs"`
					ModelID          string `json:"modelId"`
				} `json:"_meta"`
				Update struct {
					SessionUpdate string          `json:"sessionUpdate"`
					Usage         json.RawMessage `json:"usage"`
				} `json:"update"`
			} `json:"params"`
		}
		if json.Unmarshal(line, &ev) != nil {
			rep.ParseErrors++
			continue
		}
		rawUsage := ev.Params.Update.Usage
		if ev.Params.Update.SessionUpdate != "turn_completed" || len(rawUsage) == 0 || rawUsage[0] != '{' {
			continue
		}
		ts := ev.Params.Meta.AgentTimestampMs
		if ts > 10_000_000_000 || ts < -10_000_000_000 {
			ts /= 1000
		}
		inWindow := ts >= since && ts <= until && ts != 0
		credit := func(model string, d grokNums) {
			if !inWindow || (d.in == 0 && d.out == 0 && d.cache == 0) {
				return
			}
			tok := ClaudeTokens{Calls: 1, In: d.in, Out: d.out, CacheRead: d.cache}
			rep.Totals.add(tok)
			if rep.FirstTS == 0 || ts < rep.FirstTS {
				rep.FirstTS = ts
			}
			if ts > rep.LastTS {
				rep.LastTS = ts
			}
			bk := (ts / claudeSeriesStep) * claudeSeriesStep
			if cur := buckets[bk]; cur != nil {
				cur.add(tok)
			} else {
				cp := tok
				buckets[bk] = &cp
			}
			mk := model + " standard"
			byModel.add(mk, ts, tok)
			byModel.speed[mk] = "standard"
			if cwd != "" {
				byProject.add(cwd, ts, tok)
				bySession.project[sid] = cwd
			}
			if sid != "" {
				bySession.add(sid, ts, tok)
			}
		}
		var usage struct {
			ModelUsage map[string]json.RawMessage `json:"modelUsage"`
		}
		_ = json.Unmarshal(rawUsage, &usage)
		if len(usage.ModelUsage) > 0 {
			for model, raw := range usage.ModelUsage {
				cur := grokUsageNums(raw)
				d, next := grokDelta(cur, prevModels[model])
				prevModels[model] = next
				credit(model, d)
			}
			continue
		}
		cur := grokUsageNums(rawUsage)
		d, next := grokDelta(cur, prevTop)
		prevTop = next
		model := ev.Params.Meta.ModelID
		if model == "" {
			model = "grok"
		}
		credit(model, d)
	}
}
