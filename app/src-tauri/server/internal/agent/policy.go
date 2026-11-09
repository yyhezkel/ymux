package agent

// policy.go — the Block half of app/src-tauri/crates/ymux-policy
// (evaluate_with). Change both together; testdata/policy.json is read by the
// tests on both sides.
//
// Only Block is ported. Auto and Gate have no meaning here: a session's own
// policy decides those (`none` answers allow, `gate` makes a card for every
// request), so a Gate pattern would change nothing.
//
// The Rust scans the whole command, then each chained segment. The segment
// pass is left out on purpose: a segment is a substring of the command, so
// every pattern it can find, the whole-command scan already found.

import "strings"

// blockPatterns is BLOCK_PATTERNS, already in normalized form.
var blockPatterns = []string{
	// Recursive-force delete of a root-ish target.
	"rm -rf /", "rm -rf /*", "rm -rf ~", "rm -rf ~/", "rm -rf .", "rm -rf $home",
	"rm -fr /", "rm -r -f /", "rm -f -r /", "rm --recursive --force /",
	// Filesystem creation / wipe.
	"mkfs", "mke2fs", "wipefs", "fdisk", "shred ",
	// Raw writes to a block device.
	"of=/dev/sd", "of=/dev/nvme", "of=/dev/disk", "of=/dev/hd", "of=/dev/vd",
	"> /dev/sd", "> /dev/nvme",
	// chmod / chown the whole filesystem.
	"chmod -r 777 /", "chmod -r 000 /", "chmod 777 -r /", "chown -r ",
	// Database destruction.
	"drop database", "drop table", "truncate table",
	// Fork bomb.
	":(){:|:&};:",
	// Overwrite the partition table / boot sector.
	"of=/dev/mapper",
}

// normalizeCommand lowercases s and collapses every whitespace run to one
// space, so `RM   -rf /` matches `rm -rf /`.
func normalizeCommand(s string) string {
	return strings.Join(strings.Fields(strings.ToLower(s)), " ")
}

// BlockedBy returns the block pattern a tool call matches, or "" when none
// does. Only the Bash tool is evaluated. custom holds the user's extra
// patterns: they only add to the built-ins, and a blank entry matches nothing.
func BlockedBy(tool, command string, custom []string) string {
	if tool != "Bash" {
		return ""
	}
	whole := normalizeCommand(command)
	tight := strings.ReplaceAll(whole, " ", "")
	matches := func(pat string) bool {
		return pat != "" && (strings.Contains(whole, pat) ||
			strings.Contains(tight, strings.ReplaceAll(pat, " ", "")))
	}
	for _, pat := range blockPatterns {
		if matches(pat) {
			return pat
		}
	}
	for _, c := range custom {
		if pat := normalizeCommand(c); matches(pat) {
			return pat
		}
	}
	return ""
}
