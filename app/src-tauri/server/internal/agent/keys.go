package agent

// keys.go — port of translate_key in app/src-tauri/src/rpc_server.rs, the
// table behind the `send-key` RPC verb. Change both together.

import "strings"

var keyBytes = map[string]string{
	"enter": "\r", "return": "\r", "cr": "\r",
	"tab":    "\t",
	"ctrl-c": "\x03", "ctrl+c": "\x03", "^c": "\x03",
	"ctrl-d": "\x04", "ctrl+d": "\x04", "^d": "\x04",
	"ctrl-z": "\x1a", "ctrl+z": "\x1a", "^z": "\x1a",
	"ctrl-l": "\x0c", "ctrl+l": "\x0c", "^l": "\x0c",
	"esc": "\x1b", "escape": "\x1b",
	"backspace": "\x7f", "bs": "\x7f",
	"up": "\x1b[A", "arrow-up": "\x1b[A",
	"down": "\x1b[B", "arrow-down": "\x1b[B",
	"right": "\x1b[C", "arrow-right": "\x1b[C",
	"left": "\x1b[D", "arrow-left": "\x1b[D",
	"home": "\x1b[H",
	"end":  "\x1b[F",
}

// TranslateKey maps a key name to the bytes a terminal sends for it. The
// name is case-insensitive; an unknown name is sent literally — LOWERCASED,
// exactly as the Rust match falls through with the lowercased string.
func TranslateKey(name string) []byte {
	k := strings.ToLower(name)
	if b, ok := keyBytes[k]; ok {
		return []byte(b)
	}
	return []byte(k)
}
