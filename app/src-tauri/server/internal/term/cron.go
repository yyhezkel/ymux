package term

// cron.go — a 5-field cron expression for the scheduled prompts
// (schedule.go). Standard library only: Go has no cron package, and a new
// module means new go.sum lines that no dev box here can produce (Rule #17),
// the same reason pty_linux.go does not use creack/pty.
//
// Fields: minute hour day-of-month month day-of-week. Each field accepts
// `*`, `N`, `A-B`, `*/S`, `A-B/S`, `N/S` and comma lists of these. Month and
// day-of-week also accept three-letter English names (JAN, MON). Day-of-week
// 7 is Sunday, as 0 is. When both day fields are restricted, a time matches
// when EITHER matches (the Vixie cron rule). Times are the host's local time.

import (
	"errors"
	"fmt"
	"strconv"
	"strings"
	"time"
)

// cronSpec is a parsed expression: one bit per allowed value of each field.
type cronSpec struct {
	minute, hour, dom, month, dow uint64
	// domStar / dowStar: the field was `*` (or `*/S`), which changes how
	// the two day fields combine.
	domStar, dowStar bool
}

var (
	monthNames = []string{"jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"}
	dowNames   = []string{"sun", "mon", "tue", "wed", "thu", "fri", "sat"}
)

// parseCron parses a 5-field expression. It refuses anything else, including
// the 6-field (seconds) form and the @daily macros, so a typo is an error at
// add time and not a schedule that never fires.
func parseCron(expr string) (cronSpec, error) {
	f := strings.Fields(expr)
	if len(f) != 5 {
		return cronSpec{}, errors.New("cron: need 5 fields (minute hour day-of-month month day-of-week)")
	}
	var c cronSpec
	var err error
	if c.minute, _, err = parseCronField(f[0], 0, 59, nil); err != nil {
		return cronSpec{}, fmt.Errorf("cron minute: %w", err)
	}
	if c.hour, _, err = parseCronField(f[1], 0, 23, nil); err != nil {
		return cronSpec{}, fmt.Errorf("cron hour: %w", err)
	}
	if c.dom, c.domStar, err = parseCronField(f[2], 1, 31, nil); err != nil {
		return cronSpec{}, fmt.Errorf("cron day-of-month: %w", err)
	}
	if c.month, _, err = parseCronField(f[3], 1, 12, monthNames); err != nil {
		return cronSpec{}, fmt.Errorf("cron month: %w", err)
	}
	if c.dow, c.dowStar, err = parseCronField(f[4], 0, 7, dowNames); err != nil {
		return cronSpec{}, fmt.Errorf("cron day-of-week: %w", err)
	}
	if c.dow&(1<<7) != 0 { // 7 is Sunday
		c.dow = c.dow&^(1<<7) | 1
	}
	return c, nil
}

// parseCronField returns the bit set of one field and whether it started
// with `*`. names, when given, map to lo, lo+1, ... (month: JAN=1; dow: SUN=0).
func parseCronField(s string, lo, hi int, names []string) (uint64, bool, error) {
	var bits uint64
	star := strings.HasPrefix(s, "*")
	for _, part := range strings.Split(s, ",") {
		rng, stepS, hasStep := strings.Cut(part, "/")
		step := 1
		if hasStep {
			n, err := strconv.Atoi(stepS)
			if err != nil || n < 1 || n > hi-lo+1 {
				return 0, false, fmt.Errorf("bad step %q", stepS)
			}
			step = n
		}
		var a, b int
		switch {
		case rng == "*":
			a, b = lo, hi
		case strings.Contains(rng, "-"):
			as, bs, _ := strings.Cut(rng, "-")
			var err error
			if a, err = cronValue(as, lo, hi, names); err != nil {
				return 0, false, err
			}
			if b, err = cronValue(bs, lo, hi, names); err != nil {
				return 0, false, err
			}
			if a > b {
				return 0, false, fmt.Errorf("range %q runs backwards", rng)
			}
		default:
			v, err := cronValue(rng, lo, hi, names)
			if err != nil {
				return 0, false, err
			}
			a, b = v, v
			if hasStep { // `N/S` means N, N+S, ... up to the maximum
				b = hi
			}
		}
		for v := a; v <= b; v += step {
			bits |= 1 << uint(v)
		}
	}
	return bits, star, nil
}

func cronValue(s string, lo, hi int, names []string) (int, error) {
	for i, n := range names {
		if strings.EqualFold(s, n) {
			return lo + i, nil
		}
	}
	v, err := strconv.Atoi(s)
	if err != nil || v < lo || v > hi {
		return 0, fmt.Errorf("value %q out of range %d-%d", s, lo, hi)
	}
	return v, nil
}

// matches reports whether t (to the minute, in t's location) is a firing.
func (c cronSpec) matches(t time.Time) bool {
	return c.minute&(1<<uint(t.Minute())) != 0 && c.hour&(1<<uint(t.Hour())) != 0 && c.dayMatches(t)
}

// next is the first firing strictly after t, or zero when none comes within
// five years (for example `0 0 31 2 *`). Whole days that cannot match are
// skipped, so the loop stays short.
func (c cronSpec) next(t time.Time) time.Time {
	t = t.Truncate(time.Minute).Add(time.Minute)
	limit := t.AddDate(5, 0, 0)
	for t.Before(limit) {
		if !c.dayMatches(t) {
			y, m, d := t.Date()
			t = time.Date(y, m, d+1, 0, 0, 0, 0, t.Location())
			continue
		}
		if c.matches(t) {
			return t
		}
		t = t.Add(time.Minute)
	}
	return time.Time{}
}

func (c cronSpec) dayMatches(t time.Time) bool {
	if c.month&(1<<uint(t.Month())) == 0 {
		return false
	}
	domOK := c.dom&(1<<uint(t.Day())) != 0
	dowOK := c.dow&(1<<uint(t.Weekday())) != 0
	if c.domStar || c.dowStar {
		return domOK && dowOK
	}
	return domOK || dowOK
}
