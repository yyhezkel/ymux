package agent

// Translated one-for-one from the tests in app/src-tauri/src/brief.rs
// (same names, same assertions). Keep the two suites in step.

import (
	"encoding/json"
	"strings"
	"testing"
	"unicode/utf8"
)

const fullBrief = "Some analysis first.\n\n[ymux-brief]\ntask: file-lock installer\nstatus: waiting-for-you\nask: Delete the legacy lock file?\nrec: Yes — regenerated on first run.\nnext: wire into install_all()\ndelta: lock module done + tested\n"

func eqPtr(t *testing.T, field string, got *string, want string) {
	t.Helper()
	if got == nil || *got != want {
		t.Errorf("%s = %v, want %q", field, deref(got), want)
	}
}

func deref(p *string) any {
	if p == nil {
		return nil
	}
	return *p
}

func TestParsesAFullBlock(t *testing.T) {
	p, ok := ParseBrief(fullBrief)
	if !ok {
		t.Fatal("marker present")
	}
	eqPtr(t, "task", p.Task, "file-lock installer")
	if p.Status == nil || *p.Status != BriefWaitingForYou {
		t.Errorf("status = %v", p.Status)
	}
	eqPtr(t, "ask", p.Ask, "Delete the legacy lock file?")
	eqPtr(t, "rec", p.Rec, "Yes — regenerated on first run.")
	eqPtr(t, "next", p.Next, "wire into install_all()")
	eqPtr(t, "delta", p.Delta, "lock module done + tested")
}

func TestHebrewValuesSurvive(t *testing.T) {
	msg := "[ymux-brief]\nstatus: stuck\nask: למחוק את קובץ הנעילה הישן?\nrec: כן — הוא נוצר מחדש בריצה ראשונה.\n"
	p, ok := ParseBrief(msg)
	if !ok {
		t.Fatal("marker")
	}
	if p.Status == nil || *p.Status != BriefStuck {
		t.Errorf("status = %v", p.Status)
	}
	eqPtr(t, "ask", p.Ask, "למחוק את קובץ הנעילה הישן?")
	eqPtr(t, "rec", p.Rec, "כן — הוא נוצר מחדש בריצה ראשונה.")
}

func TestLastMarkerWinsOverAQuotedOne(t *testing.T) {
	msg := "Use this format:\n[ymux-brief]\ntask: EXAMPLE\n\nDone explaining.\n" + fullBrief
	p, ok := ParseBrief(msg)
	if !ok {
		t.Fatal("marker")
	}
	eqPtr(t, "task", p.Task, "file-lock installer")
	// And the pre-brief text cuts at the REAL marker, keeping the quote.
	pre := PreBriefText(msg)
	if !strings.Contains(pre, "task: EXAMPLE") || !strings.HasSuffix(pre, "Some analysis first.") {
		t.Errorf("pre-brief text = %q", pre)
	}
}

func TestMarkdownDecorationIsStripped(t *testing.T) {
	msg := "**[ymux-brief]**\n```\n- **task**: decorated\n> status: done\n```\n"
	p, ok := ParseBrief(msg)
	if !ok {
		t.Fatal("marker")
	}
	eqPtr(t, "task", p.Task, "decorated")
	if p.Status == nil || *p.Status != BriefDone {
		t.Errorf("status = %v", p.Status)
	}
}

func TestUnknownKeysAndBadStatusAreTolerated(t *testing.T) {
	p, ok := ParseBrief("[ymux-brief]\nmood: excellent\nstatus: confused\ndelta: still fine\n")
	if !ok {
		t.Fatal("marker")
	}
	if p.Status != nil {
		t.Errorf("status = %v, want nil (caller defaults to done)", *p.Status)
	}
	eqPtr(t, "delta", p.Delta, "still fine")
}

func TestStatusAliases(t *testing.T) {
	for in, want := range map[string]BriefStatus{
		"waiting": BriefWaitingForYou,
		"Blocked": BriefStuck,
		"WORKING": BriefWorking,
	} {
		if got, ok := ParseBriefStatus(in); !ok || got != want {
			t.Errorf("%q → %q,%v want %q", in, got, ok, want)
		}
	}
}

func TestNoMarkerYieldsNoneAndFullPreText(t *testing.T) {
	if _, ok := ParseBrief("just a normal answer"); ok {
		t.Error("no marker must yield none")
	}
	if got := PreBriefText("just a normal answer"); got != "just a normal answer" {
		t.Errorf("pre text = %q", got)
	}
}

func TestDegradedBriefFromPlainMessage(t *testing.T) {
	b := BriefFromStop(strPtr("Fixed the bug.\nDetails below."), strPtr("my session"), 42)
	if !b.Degraded || b.Status != BriefDone {
		t.Fatalf("brief = %+v", b)
	}
	eqPtr(t, "delta", b.Delta, "Fixed the bug.")
	eqPtr(t, "task", b.Task, "my session")
	if b.Ask != nil {
		t.Error("never fabricate a question")
	}
}

func TestDegradedBriefWithNoMessageAtAll(t *testing.T) {
	b := BriefFromStop(nil, nil, 7)
	if !b.Degraded || b.Status != BriefDone || b.Delta != nil || b.Task != nil || b.Ask != nil {
		t.Fatalf("brief = %+v", b)
	}
}

func TestParsedStatusDefaultsToDoneWhenMissing(t *testing.T) {
	b := BriefFromStop(strPtr("[ymux-brief]\ndelta: shipped\n"), nil, 1)
	if b.Degraded || b.Status != BriefDone {
		t.Fatalf("brief = %+v", b)
	}
	eqPtr(t, "delta", b.Delta, "shipped")
}

func TestLongFieldsAreClipped(t *testing.T) {
	p, ok := ParseBrief("[ymux-brief]\ndelta: " + strings.Repeat("x", 500) + "\n")
	if !ok || p.Delta == nil {
		t.Fatal("marker + delta")
	}
	if n := utf8.RuneCountInString(*p.Delta); n > 201 { // 200 + ellipsis
		t.Errorf("delta has %d chars", n)
	}
}

// Go-only: clipping counts characters, never splitting a Hebrew letter.
func TestClipCharsIsRuneSafe(t *testing.T) {
	got := ClipChars("  שלום עולם  ", 4)
	if got != "שלום…" || !utf8.ValidString(got) {
		t.Errorf("clip = %q", got)
	}
	if ClipChars(" short ", 10) != "short" {
		t.Error("an unclipped value is still trimmed")
	}
}

// Go-only: the JSON matches the desktop's `pane:brief` entry shape.
func TestBriefEntryJSONShape(t *testing.T) {
	b := BriefFromStop(nil, nil, 5)
	raw, err := json.Marshal(BriefEntry{Brief: &b, Seq: 3})
	if err != nil {
		t.Fatal(err)
	}
	want := `{"brief":{"task":null,"status":"done","ask":null,"rec":null,"next":null,"delta":null,"degraded":true,"updated_ms":5},"last_prompt":null,"prompt_ms":null,"session_ended":false,"seq":3}`
	if string(raw) != want {
		t.Errorf("json =\n%s\nwant\n%s", raw, want)
	}
}
