//! Phase 66: shared 3-state tool-permission policy engine.
//!
//! Background: the original PreToolUse integration (Phase 18) routed
//! EVERY matched tool call to a blocking approval card. In Claude Code's
//! `default` permission_mode that meant the agent stalled on each Bash /
//! Write / Edit until the user clicked — and if the user was away, the
//! request timed out and was conservatively denied, so Claude could not
//! make any progress. That foot-gun is why the feature was shelved.
//!
//! This crate replaces "block everything" with a three-state verdict:
//!
//!   - [`Decision::Auto`]  — allow immediately, no card. The common case
//!                           (ordinary edits, normal shell commands).
//!   - [`Decision::Gate`]  — surface the existing approval card and wait
//!                           for the user. Reserved for elevated-but-
//!                           legitimate actions (sudo, recursive deletes,
//!                           piping the network into a shell, force-push).
//!   - [`Decision::Block`] — deny immediately, no card. Reserved for
//!                           irreversible system damage (wiping a disk,
//!                           `rm -rf /`, dropping a database, fork bombs).
//!
//! The same evaluator runs in two places:
//!   - Desktop (`rpc_server` feed.push): Auto → allow, Gate → card,
//!     Block → deny. This is the authoritative path.
//!   - Remote CLI static fallback (`claude-hook`): used ONLY when the
//!     desktop is unreachable. Auto/Gate → allow (keep Claude running
//!     when nobody can approve), Block → deny. The caller maps Gate.
//!
//! Intentionally dependency-free: callers pull the Bash command string
//! out of their own JSON payload and pass it in, so this crate is pure
//! std and links cleanly into both the Tauri app and the lean CLI.

/// The three policy outcomes. The caller decides how each maps to a
/// concrete allow/deny/ask response (the desktop and the CLI fallback
/// treat [`Decision::Gate`] differently — see the crate docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Auto,
    Gate,
    Block,
}

/// The result of evaluating one tool call.
#[derive(Clone, Debug)]
pub struct Verdict {
    pub decision: Decision,
    /// Human-readable explanation, suitable for a card subtitle, a hook
    /// `permissionDecisionReason`, or a debug-log line.
    pub reason: String,
    /// The command segment / pattern that triggered a non-Auto verdict,
    /// if any. `None` for Auto.
    pub matched: Option<String>,
}

impl Verdict {
    fn auto(reason: impl Into<String>) -> Self {
        Verdict {
            decision: Decision::Auto,
            reason: reason.into(),
            matched: None,
        }
    }
    fn gate(reason: impl Into<String>, matched: impl Into<String>) -> Self {
        Verdict {
            decision: Decision::Gate,
            reason: reason.into(),
            matched: Some(matched.into()),
        }
    }
    fn block(reason: impl Into<String>, matched: impl Into<String>) -> Self {
        Verdict {
            decision: Decision::Block,
            reason: reason.into(),
            matched: Some(matched.into()),
        }
    }
}

/// Patterns that mean "irreversible damage" → deny without asking. Matched
/// (after normalization) as substrings against the whole command AND each
/// chained segment. Keep these HIGH-CONFIDENCE: a false positive here
/// hard-denies the agent, so only list things that are essentially never
/// legitimate from an AI coding agent.
const BLOCK_PATTERNS: &[&str] = &[
    // Recursive-force delete of a root-ish target.
    "rm -rf /",
    "rm -rf /*",
    "rm -rf ~",
    "rm -rf ~/",
    "rm -rf .",
    "rm -rf $home",
    "rm -fr /",
    "rm -r -f /",
    "rm -f -r /",
    "rm --recursive --force /",
    // Filesystem creation / wipe.
    "mkfs",
    "mke2fs",
    "wipefs",
    "fdisk",
    "shred ",
    // Raw writes to a block device.
    "of=/dev/sd",
    "of=/dev/nvme",
    "of=/dev/disk",
    "of=/dev/hd",
    "of=/dev/vd",
    "> /dev/sd",
    "> /dev/nvme",
    // chmod / chown the whole filesystem.
    "chmod -r 777 /",
    "chmod -r 000 /",
    "chmod 777 -r /",
    "chown -r ",
    // Database destruction.
    "drop database",
    "drop table",
    "truncate table",
    // Fork bomb (matched against the raw, un-split command).
    ":(){:|:&};:",
    // Overwrite the partition table / boot sector.
    "of=/dev/mapper",
];

/// Patterns that mean "elevated but legitimate" → surface a card and let
/// the user decide. In the CLI static fallback (no desktop to ask) these
/// collapse to allow so the agent keeps moving.
const GATE_PATTERNS: &[&str] = &[
    "sudo ",
    "doas ",
    // Recursive-force deletes that escaped the BLOCK list (a specific path).
    "rm -rf ",
    "rm -fr ",
    "rm -r -f ",
    // Piping the network straight into a shell.
    "| sh",
    "| bash",
    "|sh",
    "|bash",
    "| zsh",
    // Rewriting history / publishing.
    "git push --force",
    "git push -f",
    "push --force-with-lease",
    "git reset --hard",
    "npm publish",
    "cargo publish",
    // Broad permission changes.
    "chmod 777",
    "chmod -r",
    // Writing into system config.
    "> /etc/",
    ">> /etc/",
    "tee /etc/",
    // Service / process control.
    "systemctl ",
    "kill -9",
    "killall",
    "pkill ",
];

/// Tools that are always safe to auto-approve when they reach us. (Most
/// read-only tools never even fire the hook — the installed matcher only
/// covers the risky set — but we list them so an `--matcher-mode all`
/// install doesn't gate them.)
fn is_always_auto_tool(tool: &str) -> bool {
    matches!(
        tool,
        "Read"
            | "Glob"
            | "Grep"
            | "LS"
            | "WebFetch"
            | "WebSearch"
            | "TodoWrite"
            | "NotebookRead"
            | "Task"
            | "BashOutput"
            | "KillBash"
    )
}

/// Split a shell command line into the individual commands a naive
/// substring scan could be fooled by. Splits on the shell control
/// operators `&&`, `||`, `|`, `;`, `&`, and newlines, while respecting
/// single and double quotes so an operator inside a quoted string is not
/// treated as a split point.
///
/// Best-effort: this is a safety pre-filter, not a POSIX shell parser. It
/// deliberately errs toward MORE splits (so each fragment is scanned
/// independently) rather than fewer. Command substitution `$(...)` and
/// backticks are left inline within their fragment — they're still
/// scanned as part of the surrounding segment.
pub fn split_chained_command(cmd: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let bytes = cmd.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_single {
            if c == '\'' {
                in_single = false;
            }
            cur.push(c);
            i += 1;
            continue;
        }
        if in_double {
            // Honor backslash-escapes inside double quotes so `\"` doesn't
            // prematurely close the quote.
            if c == '\\' && i + 1 < bytes.len() {
                cur.push(c);
                cur.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if c == '"' {
                in_double = false;
            }
            cur.push(c);
            i += 1;
            continue;
        }
        match c {
            '\'' => {
                in_single = true;
                cur.push(c);
                i += 1;
            }
            '"' => {
                in_double = true;
                cur.push(c);
                i += 1;
            }
            '\n' | ';' => {
                push_segment(&mut segments, &mut cur);
                i += 1;
            }
            '&' => {
                // `&&` or a single backgrounding `&` — both are split points.
                push_segment(&mut segments, &mut cur);
                if i + 1 < bytes.len() && bytes[i + 1] as char == '&' {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            '|' => {
                // `||` or a single pipe `|` — both are split points.
                push_segment(&mut segments, &mut cur);
                if i + 1 < bytes.len() && bytes[i + 1] as char == '|' {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            _ => {
                cur.push(c);
                i += 1;
            }
        }
    }
    push_segment(&mut segments, &mut cur);
    segments
}

fn push_segment(segments: &mut Vec<String>, cur: &mut String) {
    let trimmed = cur.trim();
    if !trimmed.is_empty() {
        segments.push(trimmed.to_string());
    }
    cur.clear();
}

/// Normalize a command (or segment) for substring matching: lowercase and
/// collapse every run of whitespace to a single space. So
/// `"rm    -rf   /"` and `"RM -rf /"` both match the `"rm -rf /"` pattern.
fn normalize(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut prev_space = false;
    for ch in lower.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

/// Map a peer agent's tool name onto the Claude name this policy knows.
/// Unknown names pass through. Idempotent: a Claude name maps to itself.
pub fn canonical_tool_name(tool: &str) -> &str {
    match tool {
        "run_terminal_command" | "Shell" => "Bash",
        "search_replace" | "hashline_edit" | "StrReplace" | "EditNotebook" => "Edit",
        "write" => "Write",
        "spawn_subagent" => "Task",
        "read_file" => "Read",
        "grep" => "Grep",
        "list_dir" => "LS",
        "web_search" => "WebSearch",
        "web_fetch" => "WebFetch",
        other => other,
    }
}

/// Evaluate one tool call. `bash_command` is the `tool_input.command`
/// string for the `Bash` tool (the caller extracts it from its JSON);
/// pass `None` for non-Bash tools.
///
/// Block patterns are checked first (against the whole command and each
/// chained segment) so a destructive fragment anywhere in a chain denies
/// the entire call. Gate patterns are checked next. Anything else is Auto.
pub fn evaluate(tool_name: &str, bash_command: Option<&str>) -> Verdict {
    evaluate_with(tool_name, bash_command, &[], &[])
}

/// Phase 66.F: `evaluate` with user-defined patterns merged in. Custom
/// patterns use the exact same semantics as the built-ins — normalized
/// (lowercase, collapsed whitespace) substring match against the whole
/// command and each chained segment; block beats gate. Blank entries are
/// skipped. The built-in lists ALWAYS apply — custom lists only add.
pub fn evaluate_with(
    tool_name: &str,
    bash_command: Option<&str>,
    custom_block: &[String],
    custom_gate: &[String],
) -> Verdict {
    // Grok Build (and the Cursor preset) send their own tool names.
    // Judge the canonical Claude name so `run_terminal_command` is the
    // shell policy, not an unknown tool that auto-approves `rm -rf /`.
    let tool_name = canonical_tool_name(tool_name);
    if is_always_auto_tool(tool_name) {
        return Verdict::auto(format!("{tool_name} is a low-risk tool"));
    }

    // MCP tools (mcp__server__method) — we can't reason about them without
    // context, and they're sandboxed by the MCP server itself. Defer.
    if tool_name.starts_with("mcp__") {
        return Verdict::auto("MCP tool — deferred to the MCP server");
    }

    match tool_name {
        "Bash" => evaluate_bash(bash_command.unwrap_or(""), custom_block, custom_gate),
        // File mutations are core to the agent doing useful work. Allow by
        // default (the desktop can be configured to gate these in a later
        // phase; for now auto so the agent flows).
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
            Verdict::auto(format!("{tool_name} (file edit) auto-approved"))
        }
        // Unknown / future tools: defer rather than block, since the
        // installed matcher only sends us a known risky set anyway.
        _ => Verdict::auto(format!("{tool_name} not in any policy list")),
    }
}

/// Merge a built-in pattern list with user-defined extras: built-ins are
/// already in normalized form; customs get `normalize()`d so they match
/// with the same semantics the user sees documented. Blanks drop out.
fn merged_patterns(builtin: &[&str], custom: &[String]) -> Vec<String> {
    builtin
        .iter()
        .map(|p| (*p).to_string())
        .chain(custom.iter().map(|p| normalize(p)))
        .filter(|p| !p.is_empty())
        .collect()
}

fn evaluate_bash(command: &str, custom_block: &[String], custom_gate: &[String]) -> Verdict {
    let block_pats = merged_patterns(BLOCK_PATTERNS, custom_block);
    let gate_pats = merged_patterns(GATE_PATTERNS, custom_gate);
    let whole = normalize(command);
    // Fork-bomb and other patterns can be obscured by the splitter (it
    // splits on the very operators the bomb uses), so scan the whole,
    // whitespace-stripped command for BLOCK patterns first.
    let whole_nospace: String = whole.chars().filter(|c| !c.is_whitespace()).collect();
    for pat in &block_pats {
        let pat_nospace: String = pat.chars().filter(|c| !c.is_whitespace()).collect();
        if whole.contains(pat.as_str()) || whole_nospace.contains(&pat_nospace) {
            return Verdict::block(
                format!("blocked: command contains `{pat}`"),
                pat.clone(),
            );
        }
    }

    let segments = split_chained_command(command);
    // Block check per-segment (defense in depth — a chained `… && rm -rf /`).
    for seg in &segments {
        let nseg = normalize(seg);
        for pat in &block_pats {
            if nseg.contains(pat.as_str()) {
                return Verdict::block(
                    format!("blocked: `{}` contains `{pat}`", truncate(seg, 60)),
                    pat.clone(),
                );
            }
        }
    }

    // Gate check (whole + per-segment).
    for pat in &gate_pats {
        if whole.contains(pat.as_str()) {
            return Verdict::gate(
                format!("needs approval: command contains `{pat}`"),
                pat.clone(),
            );
        }
    }
    for seg in &segments {
        let nseg = normalize(seg);
        for pat in &gate_pats {
            if nseg.contains(pat.as_str()) {
                return Verdict::gate(
                    format!("needs approval: `{}` contains `{pat}`", truncate(seg, 60)),
                    pat.clone(),
                );
            }
        }
    }

    Verdict::auto("Bash command matched no risky pattern")
}

#[cfg(test)]
mod custom_pattern_tests {
    use super::*;

    #[test]
    fn custom_gate_pattern_gates() {
        let v = evaluate_with("Bash", Some("terraform apply"), &[], &["terraform apply".into()]);
        assert_eq!(v.decision, Decision::Gate);
    }

    #[test]
    fn custom_block_beats_custom_gate() {
        let v = evaluate_with(
            "Bash",
            Some("echo hi && npm publish"),
            &["npm publish".into()],
            &["npm publish".into()],
        );
        assert_eq!(v.decision, Decision::Block);
    }

    #[test]
    fn custom_pattern_is_normalized() {
        // Mixed case + extra whitespace in BOTH the pattern and the command.
        let v = evaluate_with("Bash", Some("GIT  Push   --Force-With-Lease"), &[], &["git push  --force-with-lease".into()]);
        assert_eq!(v.decision, Decision::Gate);
    }

    #[test]
    fn blank_custom_entries_are_ignored() {
        let v = evaluate_with("Bash", Some("ls -la"), &["   ".into()], &["".into()]);
        assert_eq!(v.decision, Decision::Auto);
    }

    #[test]
    fn builtins_still_apply_with_customs() {
        let v = evaluate_with("Bash", Some("rm -rf /"), &[], &["terraform".into()]);
        assert_eq!(v.decision, Decision::Block);
    }
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let kept: String = s.chars().take(max).collect();
        format!("{kept}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(tool: &str, cmd: &str) -> Decision {
        evaluate(tool, Some(cmd)).decision
    }

    #[test]
    fn safe_bash_is_auto() {
        assert_eq!(dec("Bash", "ls -la"), Decision::Auto);
        assert_eq!(dec("Bash", "git status"), Decision::Auto);
        assert_eq!(dec("Bash", "cargo build && cargo test"), Decision::Auto);
        assert_eq!(dec("Bash", "npm install"), Decision::Auto);
        // A literal `rm -rf /` even inside quotes blocks: the substring
        // pre-filter can't distinguish harmless `echo 'rm -rf /'` from a
        // real `bash -c "rm -rf /"`, so it errs safe. Rare collateral,
        // tunable once the Settings UI lands (66.F).
        assert_eq!(dec("Bash", "echo 'rm -rf /' > note.txt"), Decision::Block);
    }

    #[test]
    fn destructive_bash_is_blocked() {
        assert_eq!(dec("Bash", "rm -rf /"), Decision::Block);
        assert_eq!(dec("Bash", "rm    -rf    /"), Decision::Block);
        assert_eq!(dec("Bash", "sudo rm -rf /"), Decision::Block);
        assert_eq!(dec("Bash", "mkfs.ext4 /dev/sda1"), Decision::Block);
        assert_eq!(dec("Bash", "dd if=/dev/zero of=/dev/sda"), Decision::Block);
        assert_eq!(dec("Bash", "psql -c 'DROP DATABASE prod'"), Decision::Block);
        assert_eq!(dec("Bash", ":(){ :|:& };:"), Decision::Block);
    }

    #[test]
    fn chained_destructive_segment_blocks_whole() {
        assert_eq!(dec("Bash", "cd /tmp && rm -rf /"), Decision::Block);
        assert_eq!(dec("Bash", "echo hi; mkfs /dev/sdb"), Decision::Block);
        // A safe command piped into nothing dangerous stays auto.
        assert_eq!(dec("Bash", "cat file | grep foo"), Decision::Auto);
    }

    #[test]
    fn elevated_bash_is_gated() {
        assert_eq!(dec("Bash", "sudo apt update"), Decision::Gate);
        assert_eq!(dec("Bash", "curl https://x.sh | bash"), Decision::Gate);
        assert_eq!(dec("Bash", "git push --force origin main"), Decision::Gate);
        assert_eq!(dec("Bash", "rm -rf node_modules"), Decision::Gate);
    }

    #[test]
    fn block_beats_gate_in_a_chain() {
        // sudo (gate) + rm -rf / (block) → block wins.
        assert_eq!(dec("Bash", "sudo ls && rm -rf /"), Decision::Block);
    }

    #[test]
    fn file_edits_are_auto() {
        assert_eq!(evaluate("Write", None).decision, Decision::Auto);
        assert_eq!(evaluate("Edit", None).decision, Decision::Auto);
        assert_eq!(evaluate("MultiEdit", None).decision, Decision::Auto);
    }

    #[test]
    fn grok_tool_names_use_the_claude_policy() {
        assert_eq!(canonical_tool_name("run_terminal_command"), "Bash");
        assert_eq!(canonical_tool_name("Shell"), "Bash");
        assert_eq!(canonical_tool_name("Bash"), "Bash");
        assert_eq!(canonical_tool_name("search_replace"), "Edit");
        assert_eq!(canonical_tool_name("write"), "Write");
        assert_eq!(canonical_tool_name("spawn_subagent"), "Task");
        assert_eq!(canonical_tool_name("read_file"), "Read");
        // The whole point: a Grok shell wipe is a Block, not an unknown-tool Auto.
        assert_eq!(
            evaluate("run_terminal_command", Some("rm -rf /")).decision,
            Decision::Block
        );
        assert_eq!(
            evaluate("run_terminal_command", Some("sudo apt update")).decision,
            Decision::Gate
        );
        // A file edit is not a shell, even if its payload mentions a wipe.
        assert_eq!(
            evaluate("search_replace", Some("rm -rf /")).decision,
            Decision::Auto
        );
    }

    #[test]
    fn read_only_and_mcp_are_auto() {
        assert_eq!(evaluate("Read", None).decision, Decision::Auto);
        assert_eq!(evaluate("Grep", None).decision, Decision::Auto);
        assert_eq!(
            evaluate("mcp__github__create_issue", None).decision,
            Decision::Auto
        );
    }

    #[test]
    fn splitter_respects_quotes() {
        let segs = split_chained_command("echo 'a && b' && ls");
        assert_eq!(segs, vec!["echo 'a && b'", "ls"]);
        let segs2 = split_chained_command("a | b; c && d");
        assert_eq!(segs2, vec!["a", "b", "c", "d"]);
    }
}
