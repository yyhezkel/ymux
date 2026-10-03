package agent

import "testing"

func TestTranslateKey(t *testing.T) {
	for in, want := range map[string]string{
		"enter": "\r", "Return": "\r", "CR": "\r",
		"tab":    "\t",
		"Ctrl-C": "\x03", "ctrl+c": "\x03", "^c": "\x03",
		"ctrl-d": "\x04", "ctrl-z": "\x1a", "ctrl-l": "\x0c",
		"esc": "\x1b", "Escape": "\x1b",
		"backspace": "\x7f", "bs": "\x7f",
		"up": "\x1b[A", "arrow-down": "\x1b[B", "right": "\x1b[C", "Arrow-Left": "\x1b[D",
		"home": "\x1b[H", "end": "\x1b[F",
		// Unknown names pass through lowercased, as on the desktop.
		"q": "q", "Y": "y", "hello": "hello",
	} {
		if got := string(TranslateKey(in)); got != want {
			t.Errorf("TranslateKey(%q) = %q, want %q", in, got, want)
		}
	}
}
