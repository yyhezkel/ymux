package insights

// claudeusage.go — `GET /claude-usage`: what Claude Code actually spent on this
// machine, from the transcripts it already writes.
//
// Claude Code appends one JSON object per line to
// `~/.claude/projects/<encoded-cwd>/<session-uuid>.jsonl`, and every assistant
// line carries `message.model` plus a `message.usage` block with the real token
// counts — including the 5-minute/1-hour cache-write split, which matters
// because those two are priced differently. Nothing else on the box records
// this: `claude -p /usage` reports subscription QUOTA PERCENTAGES with no
// history, and the desktop's Claude tab has only ever shown those.
//
// This endpoint counts tokens and nothing else. It deliberately does NOT price
// them: the price table lives in the desktop (`app/src/claudePricing.ts`), in
// ONE place, so a price change is a one-file edit instead of a server rebake
// plus a matching edit in the Rust local mirror. Token counts are facts; prices
// are a table that goes stale.
//
// Why it lives in the insights package: the Monitor is the only consumer and it
// reaches every endpoint through the same `insights_fetch` command, auth, and
// SSH hop. The data is not machine metrics, which is why the file is separate.

import (
	"bufio"
	"bytes"
	"encoding/json"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"
)

// Guard rails for a directory nobody else controls the size of. On a working
// box `~/.claude/projects` is easily hundreds of MB across a hundred-plus
// transcripts, and this runs inside a `curl --max-time 6` from the desktop.
// Most lines in a transcript are user turns, attachments and tool results with
// no usage block; rejecting them on a byte scan keeps the JSON decoder off the
// hot path.
var usageMarker = []byte(`"usage"`)

const (
	claudeMaxLineSize = 8 * 1024 * 1024 // one assistant line with a big tool result
	claudeSeriesStep  = 3600            // hourly buckets, always — see below
)

// ClaudeTokens is the token tally shared by every row in the report. Cache
// writes are split because a 1-hour write costs materially more than a
// 5-minute one, and collapsing them would quietly understate a long session.
type ClaudeTokens struct {
	Calls      int   `json:"calls"`
	In         int64 `json:"in_tokens"`
	Out        int64 `json:"out_tokens"`
	CacheRead  int64 `json:"cache_read"`
	CacheWrite int64 `json:"cache_write"`
	CacheW5m   int64 `json:"cache_write_5m"`
	CacheW1h   int64 `json:"cache_write_1h"`
}

func (t *ClaudeTokens) add(o ClaudeTokens) {
	t.Calls += o.Calls
	t.In += o.In
	t.Out += o.Out
	t.CacheRead += o.CacheRead
	t.CacheWrite += o.CacheWrite
	t.CacheW5m += o.CacheW5m
	t.CacheW1h += o.CacheW1h
}

// ClaudeBucket is one point of the time series.
type ClaudeBucket struct {
	T int64 `json:"t"`
	ClaudeTokens
}

// ClaudeRow is one rollup row. `Key` is the model id, the project path, or the
// session uuid depending on which table it belongs to.
type ClaudeRow struct {
	Key string `json:"key"`
	ClaudeTokens
	// Model rows only: "standard" or "fast". Fast mode is a different price,
	// so it cannot be folded into the model row.
	Speed string `json:"speed,omitempty"`
	// Session rows only.
	Project string `json:"project,omitempty"`
	Started int64  `json:"started,omitempty"`
	Ended   int64  `json:"ended,omitempty"`
}

// ClaudeUsageReport is the whole screen in one response.
type ClaudeUsageReport struct {
	Since   int64 `json:"since"`
	Until   int64 `json:"until"`
	BucketS int64 `json:"bucket_s"`
	// Scan diagnostics — surfaced in the UI, because "$0.00" from an empty
	// scan and "$0.00" from a genuinely idle week must not look identical.
	ScannedFiles int   `json:"scanned_files"`
	SkippedFiles int   `json:"skipped_files"`
	ParseErrors  int   `json:"parse_errors"`
	TookMS       int64 `json:"took_ms"`

	Totals    ClaudeTokens   `json:"totals"`
	Sidechain ClaudeTokens   `json:"sidechain"`
	Sessions  int            `json:"sessions"`
	Projects  int            `json:"projects"`
	FirstTS   int64          `json:"first_ts"`
	LastTS    int64          `json:"last_ts"`
	Series    []ClaudeBucket `json:"series"`
	ByModel   []ClaudeRow    `json:"by_model"`
	ByProject []ClaudeRow    `json:"by_project"`
	BySession []ClaudeRow    `json:"by_session"`
}

// ─── the transcript line we care about ──────────────────────────────────

type claudeLine struct {
	Type        string `json:"type"`
	Timestamp   string `json:"timestamp"`
	SessionID   string `json:"sessionId"`
	CWD         string `json:"cwd"`
	IsSidechain bool   `json:"isSidechain"`
	Message     struct {
		Model string `json:"model"`
		Usage struct {
			InputTokens         int64 `json:"input_tokens"`
			OutputTokens        int64 `json:"output_tokens"`
			CacheReadTokens     int64 `json:"cache_read_input_tokens"`
			CacheCreationTokens int64 `json:"cache_creation_input_tokens"`
			CacheCreation       struct {
				Ephemeral5m int64 `json:"ephemeral_5m_input_tokens"`
				Ephemeral1h int64 `json:"ephemeral_1h_input_tokens"`
			} `json:"cache_creation"`
			Speed string `json:"speed"`
		} `json:"usage"`
	} `json:"message"`
}

// ClaudeProjectsDir is the transcript root. Overridable for tests.
func claudeProjectsDir() string {
	home, err := os.UserHomeDir()
	if err != nil {
		return ""
	}
	return filepath.Join(home, ".claude", "projects")
}

// accumulator keyed by an arbitrary string, so the three rollups share code.
type claudeAgg struct {
	tok     map[string]*ClaudeTokens
	speed   map[string]string
	project map[string]string
	first   map[string]int64
	last    map[string]int64
}

func newAgg() *claudeAgg {
	return &claudeAgg{
		tok:     map[string]*ClaudeTokens{},
		speed:   map[string]string{},
		project: map[string]string{},
		first:   map[string]int64{},
		last:    map[string]int64{},
	}
}

func (a *claudeAgg) add(key string, ts int64, t ClaudeTokens) {
	cur, ok := a.tok[key]
	if !ok {
		cur = &ClaudeTokens{}
		a.tok[key] = cur
		a.first[key] = ts
	}
	cur.add(t)
	if ts < a.first[key] {
		a.first[key] = ts
	}
	if ts > a.last[key] {
		a.last[key] = ts
	}
}

// rows renders the accumulator newest-cost-first. Sorted by total tokens
// because the server has no prices — the desktop re-sorts by cost once it has
// applied the table.
func (a *claudeAgg) rows(limit int) []ClaudeRow {
	out := make([]ClaudeRow, 0, len(a.tok))
	for k, t := range a.tok {
		out = append(out, ClaudeRow{
			Key:          k,
			ClaudeTokens: *t,
			Speed:        a.speed[k],
			Project:      a.project[k],
			Started:      a.first[k],
			Ended:        a.last[k],
		})
	}
	sort.Slice(out, func(i, j int) bool {
		wi := out[i].In + out[i].Out + out[i].CacheWrite
		wj := out[j].In + out[j].Out + out[j].CacheWrite
		if wi != wj {
			return wi > wj
		}
		return out[i].Key < out[j].Key
	})
	if limit > 0 && len(out) > limit {
		out = out[:limit]
	}
	return out
}

// scanClaudeUsage walks the transcript tree and aggregates one window.
//
// The mtime prune is what makes this affordable: a transcript's mtime is its
// LAST append, so a file older than `since` cannot hold an in-window line and
// is skipped without being opened. On a box with 170 transcripts and 240 MB,
// a 24-hour window typically opens a handful.
func scanClaudeUsage(root string, since, until int64) (*ClaudeUsageReport, error) {
	t0 := time.Now()
	rep := &ClaudeUsageReport{
		Since:     since,
		Until:     until,
		BucketS:   claudeSeriesStep,
		Series:    []ClaudeBucket{},
		ByModel:   []ClaudeRow{},
		ByProject: []ClaudeRow{},
		BySession: []ClaudeRow{},
	}
	if root == "" {
		return rep, nil
	}
	entries, err := os.ReadDir(root)
	if err != nil {
		// A machine that has never run Claude Code has no directory. That is
		// an empty report, not an error.
		if os.IsNotExist(err) {
			return rep, nil
		}
		return nil, err
	}

	buckets := map[int64]*ClaudeTokens{}
	byModel := newAgg()
	byProject := newAgg()
	bySession := newAgg()

	for _, dir := range entries {
		if !dir.IsDir() {
			continue
		}
		files, err := os.ReadDir(filepath.Join(root, dir.Name()))
		if err != nil {
			continue
		}
		for _, f := range files {
			if f.IsDir() || !strings.HasSuffix(f.Name(), ".jsonl") {
				continue
			}
			info, err := f.Info()
			if err != nil {
				rep.SkippedFiles++
				continue
			}
			// The prune. Last write before the window ⇒ nothing in it.
			if info.ModTime().Unix() < since {
				rep.SkippedFiles++
				continue
			}
			rep.ScannedFiles++
			path := filepath.Join(root, dir.Name(), f.Name())
			scanClaudeFile(path, since, until, rep, buckets, byModel, byProject, bySession)
		}
	}

	for t, tok := range buckets {
		rep.Series = append(rep.Series, ClaudeBucket{T: t, ClaudeTokens: *tok})
	}
	sort.Slice(rep.Series, func(i, j int) bool { return rep.Series[i].T < rep.Series[j].T })

	// The model rollup is keyed model+speed internally (fast mode is a
	// different price, so it cannot share a row); split it back apart here so
	// the desktop gets a clean model id.
	rep.ByModel = byModel.rows(20)
	for i := range rep.ByModel {
		if model, speed, ok := strings.Cut(rep.ByModel[i].Key, " "); ok {
			rep.ByModel[i].Key, rep.ByModel[i].Speed = model, speed
		}
	}
	rep.ByProject = byProject.rows(20)
	rep.BySession = bySession.rows(20)
	rep.Sessions = len(bySession.tok)
	rep.Projects = len(byProject.tok)
	rep.TookMS = time.Since(t0).Milliseconds()
	return rep, nil
}

func scanClaudeFile(
	path string, since, until int64, rep *ClaudeUsageReport,
	buckets map[int64]*ClaudeTokens, byModel, byProject, bySession *claudeAgg,
) {
	fh, err := os.Open(path)
	if err != nil {
		rep.SkippedFiles++
		return
	}
	defer fh.Close()

	sc := bufio.NewScanner(fh)
	sc.Buffer(make([]byte, 0, 256*1024), claudeMaxLineSize)
	for sc.Scan() {
		raw := sc.Bytes()
		// Cheap reject before the JSON decoder. Most lines in a transcript are
		// user turns, attachments and tool results with no usage block at all,
		// and unmarshalling them would dominate the scan.
		if !bytes.Contains(raw, usageMarker) {
			continue
		}
		var l claudeLine
		if err := json.Unmarshal(raw, &l); err != nil {
			rep.ParseErrors++
			continue
		}
		if l.Type != "assistant" || l.Message.Model == "" {
			continue
		}
		ts, err := time.Parse(time.RFC3339, l.Timestamp)
		if err != nil {
			rep.ParseErrors++
			continue
		}
		unix := ts.Unix()
		if unix < since || unix > until {
			continue
		}

		u := l.Message.Usage
		w5, w1h := u.CacheCreation.Ephemeral5m, u.CacheCreation.Ephemeral1h
		// Older transcripts have only the flat total. Attribute it to the
		// 5-minute bucket — the cheaper of the two, so an unknown split
		// under-states rather than over-states the cost.
		if w5 == 0 && w1h == 0 && u.CacheCreationTokens > 0 {
			w5 = u.CacheCreationTokens
		}
		tok := ClaudeTokens{
			Calls:      1,
			In:         u.InputTokens,
			Out:        u.OutputTokens,
			CacheRead:  u.CacheReadTokens,
			CacheWrite: u.CacheCreationTokens,
			CacheW5m:   w5,
			CacheW1h:   w1h,
		}

		rep.Totals.add(tok)
		if l.IsSidechain {
			rep.Sidechain.add(tok)
		}
		if rep.FirstTS == 0 || unix < rep.FirstTS {
			rep.FirstTS = unix
		}
		if unix > rep.LastTS {
			rep.LastTS = unix
		}

		bk := (unix / claudeSeriesStep) * claudeSeriesStep
		if cur := buckets[bk]; cur != nil {
			cur.add(tok)
		} else {
			cp := tok
			buckets[bk] = &cp
		}

		speed := u.Speed
		if speed == "" {
			speed = "standard"
		}
		// Keyed model+speed: fast mode is a different price, so the two
		// cannot share a row. A space is safe as the separator — a model id
		// never contains one, and unlike a NUL it survives every editor.
		mk := l.Message.Model + " " + speed
		byModel.add(mk, unix, tok)
		byModel.speed[mk] = speed

		if l.CWD != "" {
			byProject.add(l.CWD, unix, tok)
		}
		if l.SessionID != "" {
			bySession.add(l.SessionID, unix, tok)
			if l.CWD != "" {
				bySession.project[l.SessionID] = l.CWD
			}
		}
	}
	if err := sc.Err(); err != nil {
		rep.ParseErrors++
	}
}

// handleClaudeUsage serves GET /claude-usage?since=&until=.
//
// Same clamping contract as /analytics: the desktop drew the range picker, so
// a nonsense value is corrected rather than turned into an error page.
func (s *Service) handleClaudeUsage(w http.ResponseWriter, r *http.Request) {
	now := time.Now().Unix()
	until := parseWhen(r.URL.Query().Get("until"))
	if until <= 0 || until > now {
		until = now
	}
	since := parseWhen(r.URL.Query().Get("since"))
	if since <= 0 {
		since = until - 24*3600
	}
	// Transcripts are not swept on a schedule the way metrics are, but a scan
	// has to be bounded by something — a year is far past useful and keeps a
	// pathological request from walking every file ever written.
	if oldest := until - 365*86400; since < oldest {
		since = oldest
	}
	if newest := until - 300; since > newest {
		since = newest
	}

	rep, err := scanClaudeUsage(claudeProjectsDir(), since, until)
	if err != nil {
		logger.Error("claude usage scan failed", "err", err)
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	// Rule #1 in spirit: counts and file tallies only. Never a prompt, a
	// project path, or a session id.
	logger.Debug("claude usage ok",
		"span_s", until-since, "files", rep.ScannedFiles, "skipped", rep.SkippedFiles,
		"calls", rep.Totals.Calls, "took_ms", rep.TookMS)
	writeJSON(w, rep)
}
