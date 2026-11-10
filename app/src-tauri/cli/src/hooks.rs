//! `ymux setup-hooks` — register agent hooks that point at the local ymux CLI
//! so AI coding agents (Claude Code etc.) can pipe permission requests / lifecycle
//! events back through the tunnel into the Windows app's UI.
//!
//! Phase 18: the hook spec used to be a hardcoded `&[(event, subcmd, matcher)]`
//! slice. It now ships as a JSON file at the repo root (`hooks/<agent>.json`)
//! that the CLI fetches from raw.githubusercontent.com at install time, with
//! a `~/.ymux/cache/hooks/` fallback and the bundled spec as a final
//! last resort. The settings.json is annotated with `ymux_hooks_version` so
//! the desktop's outdated-check (also in Phase 18) can flag installs whose
//! hook spec is older than the latest published one.
//!
//! Designed to be idempotent and additive: existing entries unrelated to ymux are
//! preserved, and our entries are detected by a `ymux ... claude-hook` substring
//! match so we replace ourselves instead of accumulating duplicates. The one
//! removal: a ymux hook under an event the spec no longer names
//! (`prune_dropped_events`).

use serde::Deserialize;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

#[derive(Debug)]
pub enum AgentStatus {
    NotDetected,
    Stub(String),
    Done {
        registered: Vec<String>,
        backup: Option<PathBuf>,
        path: PathBuf,
        unchanged: bool,
        /// Phase 18: the `ymux_hooks_version` that just landed in
        /// settings.json — used by the calling print code so the user
        /// sees which version we installed.
        hooks_version: Option<String>,
        /// Events the spec no longer names, whose ymux hooks were removed.
        removed: Vec<String>,
    },
    DryRun {
        would_register: Vec<String>,
        path: PathBuf,
        already_present: Vec<String>,
        /// Events the spec no longer names, whose ymux hooks would go.
        would_remove: Vec<String>,
    },
    Error(String),
}

pub trait HookAdapter {
    fn label(&self) -> &'static str;
    fn run(&self, dry: bool, force: bool, source: &str, matcher_mode: &str) -> AgentStatus;
}

/// Phase 18.1: rewrite the loaded spec's matchers based on the user's
/// `matcher_mode` choice. `restrictive` is a passthrough (whatever the
/// hook spec already says, typically the narrow risky-tool regex);
/// `all` rewrites every event's matcher to `.*` so EVERY tool call
/// surfaces a ymux card — useful for debugging missing-card issues;
/// `custom` is a passthrough too (the caller is hand-managing
/// settings.json and we should leave the spec intact). We only ever
/// override matchers on events the spec actually declared — we don't
/// invent new events.
pub fn apply_matcher_mode(spec: &mut HookSpec, matcher_mode: &str) {
    match matcher_mode {
        "all" => {
            for (_, ev) in spec.events.iter_mut() {
                ev.matcher = ".*".into();
            }
        }
        // "restrictive" / "custom" / unknown → leave spec untouched.
        _ => {}
    }
}

// ─── Hook spec (the shape of hooks/<agent>.json) ───────────────────────────

/// Parsed spec for one agent. Matches the JSON in `hooks/<agent>.json`.
#[derive(Clone, Debug, Deserialize)]
pub struct HookSpec {
    pub ymux_hooks_version: String,
    #[allow(dead_code)]
    pub agent: String,
    #[serde(default)]
    pub events: std::collections::BTreeMap<String, HookEvent>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct HookEvent {
    pub matcher: String,
    pub command: String,
    /// Phase 86: per-hook timeout in seconds, written through to the
    /// agent's settings verbatim. Claude Code kills a hook after 60s by
    /// default; the desktop Gate waits up to 120s (600 clamp), so
    /// PreToolUse needs more or Claude Code gives up on it first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
}

/// One settings entry for an event: `{ matcher, hooks: [{type, command[, timeout]}] }`.
fn hook_entry(matcher: &str, command: &str, timeout: Option<u64>) -> Value {
    let mut hook = json!({ "type": "command", "command": command });
    if let Some(t) = timeout {
        hook["timeout"] = json!(t);
    }
    json!({ "matcher": matcher, "hooks": [hook] })
}

/// The version this CLI was built with, used as the spec returned by
/// `source=bundled` AND as the version recorded when no fetched spec
/// was applied. Bump whenever you ship a new hook in a release with a
/// matching `hooks/claude-code.json` change.
const BUNDLED_CLAUDE_VERSION: &str = "1.8.0";

/// The bundled fallback spec for Claude Code. Mirrors what
/// `hooks/claude-code.json` carries at the same `ymux_hooks_version`
/// — kept in sync by hand for now (a `build.rs` that bakes the file
/// into the binary is on the roadmap).
fn bundled_claude_spec() -> HookSpec {
    use std::collections::BTreeMap;
    let mut events: BTreeMap<String, HookEvent> = BTreeMap::new();
    events.insert(
        "PreToolUse".into(),
        HookEvent {
            matcher: "Bash|Write|Edit|MultiEdit|NotebookEdit|Task".into(),
            command: "${YMUX_BIN} claude-hook pre-tool-use".into(),
            // v1.6.0 (Phase 86): outlive Claude Code's 60s default.
            timeout: Some(600),
        },
    );
    // v0.4.4 dropped Notification and SessionStart as observability-only
    // noise. v1.7.0 (Phase 105.C) registers SessionStart again for ONE job:
    // the CLI asks the desktop for this session's context and prints it as
    // `additionalContext` (compact/resume/startup). It still never becomes
    // a feed item or a toast — see the session-start branch in main.rs.
    //
    // v1.5.0 (Phase 84.B) brings Notification BACK, with a different job.
    // It was noise as a feed item — a card per event that nobody acted on
    // — but it is the only signal Claude Code emits for "the agent is
    // blocked on the human", which is the red light on a pane. The CLI
    // filters on notification_type and pushes the type alone; the desktop
    // routes it to per-pane state and never to the feed or a toast.
    // UserPromptSubmit (v1.3.0, issue #4): turn-start signal for the
    // ymux-tools chrome Ticker. Fires in every permission mode (unlike
    // pre-tool-use, which the CLI short-circuits in acceptEdits/bypass).
    // The desktop keeps it off the feed — it only stamps per-pane turn timing.
    // v1.8.0: StopFailure (turn died on an API error) — the CLI pushes the
    // error type alone; the desktop paints the pane light `failed`, state
    // only, never a feed item or a toast.
    for (ev, sub) in [
        ("Notification", "notification"),
        ("SessionEnd", "session-end"),
        ("SessionStart", "session-start"),
        ("Stop", "stop"),
        ("StopFailure", "stop-failure"),
        ("UserPromptSubmit", "user-prompt-submit"),
    ] {
        events.insert(
            ev.into(),
            HookEvent {
                // Lifecycle events aren't tool-matched; an empty matcher is
                // the documented "applies always" form. `"*"` is NOT a
                // documented wildcard (it's a literal), so avoid it.
                matcher: "".into(),
                command: format!("${{YMUX_BIN}} claude-hook {sub}"),
                timeout: None,
            },
        );
    }
    HookSpec {
        ymux_hooks_version: BUNDLED_CLAUDE_VERSION.into(),
        agent: "claude-code".into(),
        events,
    }
}

/// Resolve the spec per `--source`:
///   - `github`: fetch `raw.githubusercontent.com/yyhezkel/ymux/main/hooks/<agent>.json`,
///     fall through to cache, then to bundled.
///   - `bundled`: skip the network entirely.
///   - `url=<u>`: fetch from a custom URL, no cache fallback (the user
///     is opting into a specific source — silently swapping to bundled
///     would surprise them).
/// On every successful fetch, write the JSON to
/// `~/.ymux/cache/hooks/<agent>.json` so the next call without
/// network connectivity still picks up the latest spec.
pub fn load_spec(source: &str, agent_id: &str) -> Result<HookSpec, String> {
    let canonical_url = format!(
        "https://raw.githubusercontent.com/yyhezkel/ymux/main/hooks/{agent_id}.json"
    );

    match source {
        "bundled" => Ok(bundled_spec_for(agent_id)),
        "github" => {
            match fetch_url(&canonical_url) {
                Ok(text) => {
                    let spec = parse_spec(&text)?;
                    let _ = write_cache(agent_id, &text);
                    Ok(spec)
                }
                Err(e_fetch) => {
                    eprintln!("setup-hooks: github fetch failed ({e_fetch}) — trying cache");
                    if let Ok(text) = read_cache(agent_id) {
                        if let Ok(spec) = parse_spec(&text) {
                            eprintln!("setup-hooks: using cached spec");
                            return Ok(spec);
                        }
                    }
                    eprintln!("setup-hooks: cache miss — using bundled spec");
                    Ok(bundled_spec_for(agent_id))
                }
            }
        }
        s if s.starts_with("url=") => {
            let u = &s[4..];
            let text = fetch_url(u)?;
            let spec = parse_spec(&text)?;
            let _ = write_cache(agent_id, &text);
            Ok(spec)
        }
        other => Err(format!(
            "unknown --source {other:?} (expected github / bundled / url=<U>)"
        )),
    }
}

fn parse_spec(text: &str) -> Result<HookSpec, String> {
    serde_json::from_str(text.trim_start_matches('\u{FEFF}'))
        .map_err(|e| format!("parse hook spec: {e}"))
}

fn bundled_spec_for(agent_id: &str) -> HookSpec {
    match agent_id {
        "claude-code" | "claude" => bundled_claude_spec(),
        // Other agents have no bundled fallback — return an empty
        // spec so the caller's apply step is a no-op. The github
        // path is the only way they get useful hooks today.
        other => HookSpec {
            ymux_hooks_version: "0.0.0".into(),
            agent: other.into(),
            events: Default::default(),
        },
    }
}

/// Shell out to curl / wget for the fetch. Both are universally
/// present on the Linux servers we target, and on Windows 10+ curl
/// ships in the base OS. We deliberately avoid pulling in `reqwest`
/// (the dep tree adds ~2 MB to the CLI binary for one HTTP GET).
fn fetch_url(url: &str) -> Result<String, String> {
    let curl = std::process::Command::new("curl")
        .args(["-fsSL", "--max-time", "10", url])
        .output();
    match curl {
        Ok(o) if o.status.success() => {
            return Ok(String::from_utf8_lossy(&o.stdout).to_string());
        }
        Ok(o) => {
            // curl ran but returned non-zero. Try wget too before giving up.
            let curl_err = String::from_utf8_lossy(&o.stderr).trim().to_string();
            if let Some(out) = try_wget(url) {
                return Ok(out);
            }
            return Err(format!("curl exit {}: {curl_err}", o.status));
        }
        Err(_) => {
            if let Some(out) = try_wget(url) {
                return Ok(out);
            }
        }
    }
    Err("neither curl nor wget is available".into())
}

fn try_wget(url: &str) -> Option<String> {
    let out = std::process::Command::new("wget")
        .args(["-q", "-O", "-", "--timeout=10", url])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

fn cache_path(agent_id: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(
        PathBuf::from(home)
            .join(".ymux")
            .join("cache")
            .join("hooks")
            .join(format!("{agent_id}.json")),
    )
}

fn write_cache(agent_id: &str, text: &str) -> Result<(), String> {
    let path = cache_path(agent_id).ok_or_else(|| "no $HOME".to_string())?;
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(&path, text).map_err(|e| format!("cache write {path:?}: {e}"))
}

fn read_cache(agent_id: &str) -> Result<String, String> {
    let path = cache_path(agent_id).ok_or_else(|| "no $HOME".to_string())?;
    fs::read_to_string(&path).map_err(|e| format!("cache read {path:?}: {e}"))
}

// ─── Claude adapter ────────────────────────────────────────────────────────

pub struct Claude;

impl HookAdapter for Claude {
    fn label(&self) -> &'static str {
        "Claude Code"
    }

    fn run(&self, dry: bool, force: bool, source: &str, matcher_mode: &str) -> AgentStatus {
        // Phase 80: USERPROFILE fallback — `ymux setup-hooks` now also
        // runs on native Windows (the app's local smart-install step),
        // where only USERPROFILE is set.
        let home = match std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            Some(h) => h,
            None => return AgentStatus::Error("$HOME / %USERPROFILE% not set".into()),
        };
        let claude_dir = PathBuf::from(&home).join(".claude");
        if !claude_dir.is_dir() {
            return AgentStatus::NotDetected;
        }

        // Phase 80: on Windows the hook command must point at THIS
        // ymux-cli.exe (there is no ~/.ymux/bin/ymux — the CLI
        // ships next to the app). Double-quote it inside the command
        // string: it typically lives under "C:\Program Files\ymux\…"
        // and Claude Code executes hook commands through a shell.
        let exe_path = if cfg!(windows) {
            match std::env::current_exe() {
                Ok(p) => format!("\"{}\"", p.to_string_lossy()),
                Err(e) => return AgentStatus::Error(format!("current_exe: {e}")),
            }
        } else {
            match home.to_str() {
                Some(s) => format!("{}/.ymux/bin/ymux", s),
                None => return AgentStatus::Error("non-UTF-8 $HOME".into()),
            }
        };

        let mut spec = match load_spec(source, "claude-code") {
            Ok(s) => s,
            Err(e) => return AgentStatus::Error(format!("load spec: {e}")),
        };
        apply_matcher_mode(&mut spec, matcher_mode);

        // Phase setup-hooks-fix: current Claude Code reads hooks from
        // `~/.claude/settings.json` under a top-level `"hooks"` key — NOT
        // from a separate `~/.claude/hooks.json`. We write BOTH so:
        //   • settings.json is what Claude Code actually consumes,
        //   • hooks.json stays for any legacy tooling that might still read it.
        let settings_path = claude_dir.join("settings.json");
        let legacy_path = claude_dir.join("hooks.json");

        let settings_outcome =
            apply_to_settings(&claude_dir, &settings_path, &exe_path, &spec, dry, force);
        let _ = apply_to_legacy(&claude_dir, &legacy_path, &exe_path, &spec, dry, force);
        settings_outcome
    }
}

/// Substitute `${YMUX_BIN}` (and the legacy bare `ymux`) in a
/// spec command string with the absolute path we just computed.
///
/// `${WINMUX_BIN}` is still expanded: a hook spec cached under
/// `~/.ymux/cache/hooks/` (or fetched from a raw.githubusercontent URL
/// that hasn't been re-published yet) can predate the rename, and an
/// unexpanded placeholder would be handed to the shell verbatim.
fn expand_command(cmd: &str, exe_path: &str) -> String {
    cmd.replace("${YMUX_BIN}", exe_path)
        .replace("${WINMUX_BIN}", exe_path)
}

fn apply_to_settings(
    claude_dir: &std::path::Path,
    path: &std::path::Path,
    exe_path: &str,
    spec: &HookSpec,
    dry: bool,
    force: bool,
) -> AgentStatus {
    let mut root: Value = match fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str(text.trim_start_matches('\u{FEFF}')) {
            Ok(v) => v,
            Err(e) => return AgentStatus::Error(format!("parse {}: {}", path.display(), e)),
        },
        Err(_) => json!({}),
    };
    if !root.is_object() {
        return AgentStatus::Error(format!(
            "{} is not a JSON object (got {:?}); refusing to overwrite",
            path.display(),
            kind_of(&root)
        ));
    }
    if !root.get("hooks").is_some_and(|h| h.is_object()) {
        root["hooks"] = json!({});
    }

    // In memory only until the write below; a dry run reports it and stops.
    let dropped = match root.get_mut("hooks").and_then(Value::as_object_mut) {
        Some(hooks) => prune_dropped_events(hooks, spec),
        None => Vec::new(),
    };

    let mut would_register: Vec<String> = Vec::new();
    let mut already_present: Vec<String> = Vec::new();
    let mut to_apply: Vec<(String, String, String, Option<u64>)> = Vec::new();

    for (event, ev_spec) in &spec.events {
        let cmd = expand_command(&ev_spec.command, exe_path);
        let entries = root["hooks"][event]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let has_ymux = entries.iter().any(is_ymux_entry);
        // Phase 84.B: "present" is not enough — the command has to still
        // exist, or a rename leaves a permanently dead hook that
        // setup-hooks refuses to repair. See ymux_entry_is_runnable.
        if has_ymux
            && !force
            && ymux_entry_is_runnable(&entries)
            && ymux_entry_points_at(&entries, exe_path)
            && ymux_entry_has_timeout(&entries, ev_spec.timeout)
        {
            already_present.push(event.clone());
            continue;
        }
        would_register.push(event.clone());
        to_apply.push((event.clone(), cmd, ev_spec.matcher.clone(), ev_spec.timeout));
    }

    if dry {
        return AgentStatus::DryRun {
            would_register,
            path: path.to_path_buf(),
            already_present,
            would_remove: dropped,
        };
    }

    if to_apply.is_empty() && dropped.is_empty() {
        // Even when no event entries change, refresh the meta tag so
        // the desktop's outdated-check picks up a version bump that's
        // purely a no-op (e.g. a spec rebuild with identical events).
        if root["ymux_meta"]
            .get("hooks_version")
            .and_then(|v| v.as_str())
            != Some(spec.ymux_hooks_version.as_str())
        {
            root["ymux_meta"] = json!({
                "hooks_version": spec.ymux_hooks_version,
                "agent": spec.agent,
            });
            if let Err(e) = write_settings_atomic(claude_dir, path, &root) {
                return AgentStatus::Error(e);
            }
        }
        return AgentStatus::Done {
            registered: vec![],
            backup: None,
            path: path.to_path_buf(),
            unchanged: true,
            hooks_version: Some(spec.ymux_hooks_version.clone()),
            removed: vec![],
        };
    }

    let backup = if path.exists() {
        let stamp = chrono::Local::now().format("%Y%m%dT%H%M%S").to_string();
        let bk = claude_dir.join(format!("settings.json.bak.{}", stamp));
        if let Err(e) = fs::copy(path, &bk) {
            return AgentStatus::Error(format!("backup {}: {}", bk.display(), e));
        }
        Some(bk)
    } else {
        None
    };

    for (event, cmd, matcher, timeout) in &to_apply {
        let mut entries = root["hooks"][event.as_str()]
            .as_array()
            .cloned()
            .unwrap_or_default();
        entries.retain(|e| !is_ymux_entry(e));
        entries.push(hook_entry(matcher, cmd, *timeout));
        root["hooks"][event.as_str()] = json!(entries);
    }

    // Phase 18: stamp the version into settings.json so the desktop's
    // outdated check has somewhere to read it back from. Lives under
    // a sibling `ymux_meta` key so we don't risk colliding with
    // anything Claude Code itself adds to its config.
    root["ymux_meta"] = json!({
        "hooks_version": spec.ymux_hooks_version,
        "agent": spec.agent,
    });
    // Retire the pre-rename sibling rather than leaving a second, now
    // permanently stale, version stamp next to the live one.
    if let Some(obj) = root.as_object_mut() {
        obj.remove("winmux_meta");
    }

    if let Err(e) = write_settings_atomic(claude_dir, path, &root) {
        return AgentStatus::Error(e);
    }

    AgentStatus::Done {
        registered: to_apply.into_iter().map(|(e, _, _, _)| e).collect(),
        backup,
        path: path.to_path_buf(),
        unchanged: false,
        hooks_version: Some(spec.ymux_hooks_version.clone()),
        removed: dropped,
    }
}

/// The one writer of `settings.json`: a tmp file in the same directory, then
/// a rename (Rule #7). Claude Code reads this file, and a write cut short
/// would leave it truncated.
fn write_settings_atomic(
    claude_dir: &std::path::Path,
    path: &std::path::Path,
    root: &Value,
) -> Result<(), String> {
    let text = serde_json::to_string_pretty(root).map_err(|e| format!("serialize: {e}"))?;
    let tmp = claude_dir.join(format!("settings.json.ymux-tmp.{}", std::process::id()));
    fs::write(&tmp, &text).map_err(|e| format!("write tmp: {e}"))?;
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("rename: {e}"));
    }
    Ok(())
}

/// Remove ymux hook commands from the events `spec` does not name, and
/// return those events. The install loop visits spec events only, so an
/// event dropped from the spec kept its entry for good and ran the CLI on
/// every firing.
///
/// Only a hook object whose own command is ours goes. An entry is dropped
/// only when that leaves its `hooks` array empty, and an event key only
/// when that leaves it with no entries, so a user's hook that shares the
/// event or the entry stays. A spec with no events prunes nothing: it is
/// the empty fallback or a broken fetch, not a decision to drop them all.
fn prune_dropped_events(
    hooks: &mut serde_json::Map<String, Value>,
    spec: &HookSpec,
) -> Vec<String> {
    if spec.events.is_empty() {
        return Vec::new();
    }
    let mut removed: Vec<String> = Vec::new();
    let mut emptied_events: Vec<String> = Vec::new();
    for (event, entries) in hooks.iter_mut() {
        if spec.events.contains_key(event) {
            continue;
        }
        let Some(list) = entries.as_array_mut() else {
            continue;
        };
        let mut touched = false;
        let mut kept: Vec<Value> = Vec::with_capacity(list.len());
        for mut entry in std::mem::take(list) {
            let mut emptied = false;
            if let Some(cmds) = entry.get_mut("hooks").and_then(Value::as_array_mut) {
                let before = cmds.len();
                cmds.retain(|h| {
                    !h.get("command")
                        .and_then(|c| c.as_str())
                        .is_some_and(is_ymux_command)
                });
                if cmds.len() != before {
                    touched = true;
                    emptied = cmds.is_empty();
                }
            }
            if !emptied {
                kept.push(entry);
            }
        }
        if touched {
            removed.push(event.clone());
            if kept.is_empty() {
                emptied_events.push(event.clone());
            }
        }
        *list = kept;
    }
    for event in &emptied_events {
        hooks.remove(event);
    }
    removed
}

fn apply_to_legacy(
    claude_dir: &std::path::Path,
    path: &std::path::Path,
    exe_path: &str,
    spec: &HookSpec,
    dry: bool,
    force: bool,
) -> AgentStatus {
    let mut config: Value = match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(text.trim_start_matches('\u{FEFF}')).unwrap_or(json!({})),
        Err(_) => json!({}),
    };
    if !config.is_object() {
        config = json!({});
    }

    let mut to_apply: Vec<(String, String, String, Option<u64>)> = Vec::new();
    for (event, ev_spec) in &spec.events {
        let cmd = expand_command(&ev_spec.command, exe_path);
        let entries = config[event].as_array().cloned().unwrap_or_default();
        let has_ymux = entries.iter().any(is_ymux_entry);
        if has_ymux
            && !force
            && ymux_entry_points_at(&entries, exe_path)
            && ymux_entry_has_timeout(&entries, ev_spec.timeout)
        {
            continue;
        }
        to_apply.push((event.clone(), cmd, ev_spec.matcher.clone(), ev_spec.timeout));
    }
    if dry || to_apply.is_empty() {
        return AgentStatus::Done {
            registered: vec![],
            backup: None,
            path: path.to_path_buf(),
            unchanged: true,
            hooks_version: Some(spec.ymux_hooks_version.clone()),
            removed: vec![],
        };
    }

    let backup = if path.exists() {
        let stamp = chrono::Local::now().format("%Y%m%dT%H%M%S").to_string();
        let bk = claude_dir.join(format!("hooks.json.bak.{}", stamp));
        let _ = fs::copy(path, &bk);
        Some(bk)
    } else {
        None
    };

    for (event, cmd, matcher, timeout) in &to_apply {
        let mut entries = config[event.as_str()]
            .as_array()
            .cloned()
            .unwrap_or_default();
        entries.retain(|e| !is_ymux_entry(e));
        entries.push(hook_entry(matcher, cmd, *timeout));
        config[event.as_str()] = json!(entries);
    }

    let text = serde_json::to_string_pretty(&config).unwrap_or_default();
    let _ = fs::write(path, text);

    AgentStatus::Done {
        registered: to_apply.into_iter().map(|(e, _, _, _)| e).collect(),
        backup,
        path: path.to_path_buf(),
        unchanged: false,
        hooks_version: Some(spec.ymux_hooks_version.clone()),
        removed: vec![],
    }
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn is_ymux_entry(entry: &Value) -> bool {
    entry
        .get("hooks")
        .and_then(|v| v.as_array())
        .map(|hooks| {
            hooks.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .is_some_and(is_ymux_command)
            })
        })
        .unwrap_or(false)
}

/// Is this hook command one of ours?
///
/// Both spellings. "winmux" does NOT contain "ymux" as a substring, so
/// matching only the new name would leave every pre-rename entry in place
/// and then push a second one beside it — two hooks firing per event.
fn is_ymux_command(c: &str) -> bool {
    (c.contains("ymux") || c.contains("winmux")) && c.contains("claude-hook")
}

/// Phase 84.B: is the ymux entry for this event actually runnable?
///
/// `is_ymux_entry` matches on the command *text*, so an entry naming a
/// binary that no longer exists still counts as "already present" and
/// `setup-hooks` skips it forever. That is not hypothetical: the
/// winmux → ymux rename moved the binary, and every settings.json written
/// before it kept pointing at the old absolute path. The hook then fails
/// silently on every invocation — Claude Code does not surface a missing
/// hook binary — so the desktop simply never hears from that agent again.
///
/// Deliberately conservative. Only a ROOTED path that provably is not on
/// disk counts as dead. A bare command name resolves through PATH, a
/// relative path resolves against a working directory we don't know, and
/// an unexpanded `${VAR}` resolves at run time; none can be checked here,
/// and guessing wrong would rewrite a working hook on every single run.
///
/// `is_absolute() || has_root()` rather than `is_absolute()` alone: on
/// Windows a POSIX-style `/home/y/.winmux/bin/winmux` is NOT absolute
/// (no drive prefix) but is rooted, and settings.json written under WSL
/// or copied between machines carries exactly that shape.
/// Phase 86: "present" also means "carries the spec's `timeout`". Without
/// this, a 1.5.0 install would keep its 60s-default PreToolUse entry
/// forever — the skip path never rewrites an entry it considers present,
/// yet the version stamp would still move to 1.6.0 and silence the
/// outdated-hooks banner. Only checks the spec's direction (a spec with no
/// timeout accepts any entry).
/// Phase 86: "present" also means "points at THIS binary". Found live on
/// Yossi's server: 4 of 5 entries still ran `~/.winmux/bin/winmux` (a
/// pre-rename build with no last.env fallback, failing on every prompt
/// with `connect tcp 127.0.0.1:<dead port>`), the file existed so
/// `ymux_entry_is_runnable` was happy, and the stamp said 1.5.0. The
/// compat symlink keeps the old binary alive forever, so the only way an
/// install converges on the current CLI is to rewrite any of our entries
/// whose executable differs from the one being installed.
fn ymux_entry_points_at(entries: &[Value], exe_path: &str) -> bool {
    let norm = |p: &str| p.replace('\\', "/").trim_matches('"').to_string();
    let want = norm(exe_path);
    entries.iter().filter(|e| is_ymux_entry(e)).all(|entry| {
        entry
            .get("hooks")
            .and_then(|v| v.as_array())
            .map(|hooks| {
                hooks.iter().all(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .and_then(executable_token)
                        .map(|exe| norm(&exe) == want)
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    })
}

fn ymux_entry_has_timeout(entries: &[Value], want: Option<u64>) -> bool {
    let Some(want) = want else { return true };
    entries.iter().filter(|e| is_ymux_entry(e)).all(|entry| {
        entry
            .get("hooks")
            .and_then(|v| v.as_array())
            .map(|hooks| hooks.iter().all(|h| h.get("timeout").and_then(|t| t.as_u64()) == Some(want)))
            .unwrap_or(false)
    })
}

fn ymux_entry_is_runnable(entries: &[Value]) -> bool {
    entries.iter().filter(|e| is_ymux_entry(e)).all(|entry| {
        entry
            .get("hooks")
            .and_then(|v| v.as_array())
            .map(|hooks| {
                hooks.iter().all(|h| {
                    let Some(cmd) = h.get("command").and_then(|c| c.as_str()) else {
                        return true;
                    };
                    match executable_token(cmd) {
                        Some(exe) => {
                            let p = std::path::Path::new(&exe);
                            let rooted = p.is_absolute() || p.has_root();
                            !rooted || p.exists()
                        }
                        None => true,
                    }
                })
            })
            .unwrap_or(true)
    })
}

/// The executable token of a shell command line: the leading quoted path
/// if there is one, otherwise everything up to the first space. Returns
/// None for anything containing a shell variable, since the real target
/// is only known at run time.
fn executable_token(cmd: &str) -> Option<String> {
    let cmd = cmd.trim();
    if let Some(rest) = cmd.strip_prefix('"') {
        let (exe, _) = rest.split_once('"')?;
        return if exe.contains('$') || exe.contains('%') {
            None
        } else {
            Some(exe.to_string())
        };
    }
    let exe = cmd.split(' ').next()?;
    if exe.is_empty() || exe.contains('$') || exe.contains('%') {
        return None;
    }
    Some(exe.to_string())
}

pub struct Stub {
    pub label: &'static str,
}

impl HookAdapter for Stub {
    fn label(&self) -> &'static str {
        self.label
    }
    fn run(&self, _dry: bool, _force: bool, _source: &str, _matcher_mode: &str) -> AgentStatus {
        AgentStatus::Stub("not yet implemented".into())
    }
}

pub fn run_all(
    adapters: &[Box<dyn HookAdapter>],
    dry: bool,
    force: bool,
    source: &str,
    matcher_mode: &str,
) {
    for a in adapters {
        let s = a.run(dry, force, source, matcher_mode);
        print_status(a.label(), &s);
    }
    if dry {
        println!("Done. (dry-run, no writes)");
    } else {
        println!("Done. Restart your agent for hooks to take effect.");
    }
}

fn print_status(label: &str, status: &AgentStatus) {
    match status {
        AgentStatus::NotDetected => {
            println!("✗ {}: not detected (skipped)", label);
        }
        AgentStatus::Stub(reason) => {
            println!("✗ {}: {}", label, reason);
        }
        AgentStatus::Error(e) => {
            println!("✗ {}: error — {}", label, e);
        }
        AgentStatus::DryRun {
            would_register,
            path,
            already_present,
            would_remove,
        } => {
            println!("✓ {} detected", label);
            if !would_remove.is_empty() {
                println!(
                    "  → would remove ymux hooks no longer in the spec: {}",
                    would_remove.join(", ")
                );
            }
            if !already_present.is_empty() {
                println!(
                    "  → already present (would skip): {}",
                    already_present.join(", ")
                );
            }
            if would_register.is_empty() {
                println!("  → all hooks already present, would skip ({})", path.display());
            } else {
                println!(
                    "  → would register: {} (target: {})",
                    would_register.join(", "),
                    path.display()
                );
                println!("  → would create a timestamped .bak before writing");
            }
        }
        AgentStatus::Done {
            registered,
            backup,
            path,
            unchanged,
            hooks_version,
            removed,
        } => {
            println!("✓ {} detected", label);
            if *unchanged {
                println!("  → all hooks already present, nothing to do ({})", path.display());
            } else {
                if !registered.is_empty() {
                    println!(
                        "  → registered: {} (target: {})",
                        registered.join(", "),
                        path.display()
                    );
                }
                if !removed.is_empty() {
                    println!(
                        "  → removed ymux hooks no longer in the spec: {} (target: {})",
                        removed.join(", "),
                        path.display()
                    );
                }
                if let Some(bk) = backup {
                    println!("  → backed up previous to {}", bk.display());
                }
            }
            if let Some(v) = hooks_version {
                println!("  → ymux_hooks_version = {v}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── winmux → ymux rename ───────────────────────────────────────
    //
    // Both assertions below cover the same trap, which the compiler
    // cannot see: the substring "winmux" DOES NOT CONTAIN "ymux".
    // Every pre-rename hook command and every pre-rename settings.json
    // says winmux, so a check written against the new name alone is
    // silently blind to exactly the installs that need handling.

    fn entry(command: &str) -> Value {
        json!({
            "matcher": "Bash",
            "hooks": [{ "type": "command", "command": command }]
        })
    }

    // Pins the v1.8.0 registration: dropping StopFailure from the bundled
    // spec would silently leave the pane light yellow on API errors.
    #[test]
    fn bundled_spec_registers_stop_failure() {
        let spec = bundled_claude_spec();
        assert_eq!(spec.ymux_hooks_version, "1.8.0");
        assert_eq!(spec.events.len(), 7);
        let ev = spec.events.get("StopFailure").expect("StopFailure registered");
        assert_eq!(ev.matcher, "");
        assert_eq!(ev.command, "${YMUX_BIN} claude-hook stop-failure");
    }

    #[test]
    fn ours_is_recognised_under_either_name() {
        assert!(is_ymux_entry(&entry("/home/y/.ymux/bin/ymux claude-hook pre-tool-use")));
        // The one that mattered: left unmatched, `retain` keeps this entry
        // and then pushes a second one beside it — two permission cards
        // per tool call on every machine that ran the old build.
        assert!(is_ymux_entry(&entry("/home/y/.winmux/bin/winmux claude-hook pre-tool-use")));
    }

    #[test]
    fn someone_elses_hook_is_left_alone() {
        // `claude-hook` is half the predicate; a third-party hook that
        // merely lives under our directory is not ours to rewrite.
        assert!(!is_ymux_entry(&entry("/usr/local/bin/other-tool run")));
        assert!(!is_ymux_entry(&entry("/home/y/.ymux/bin/some-other-thing --flag")));
    }

    #[test]
    fn both_bin_placeholders_expand() {
        let exe = "/home/y/.ymux/bin/ymux";
        assert_eq!(
            expand_command("${YMUX_BIN} claude-hook stop", exe),
            "/home/y/.ymux/bin/ymux claude-hook stop"
        );
        // A spec cached under ~/.ymux/cache/hooks/ before the rename still
        // carries the old placeholder; unexpanded, it reaches the shell
        // verbatim and the hook silently never fires.
        assert_eq!(
            expand_command("${WINMUX_BIN} claude-hook stop", exe),
            "/home/y/.ymux/bin/ymux claude-hook stop"
        );
    }

    // ── Phase 84.B: the dead-hook check ──────────────────────────────

    // Built from temp_dir so the path is genuinely absolute on the host
    // running the test. A literal "/nonexistent/..." is NOT absolute on
    // Windows — no drive prefix — so it would sail through the check and
    // pass this test for the wrong reason on the one platform CI gates on.
    fn dead_path(tail: &str) -> String {
        std::env::temp_dir()
            .join("ymux-nonexistent-hook-test")
            .join(tail)
            .display()
            .to_string()
    }

    #[test]
    fn an_absolute_path_that_is_gone_is_not_runnable() {
        // The winmux -> ymux rename in one line: the entry still reads as
        // ours, so the old skip path called it "already present" and never
        // repaired it. This directory has never existed.
        let dead = entry(&format!("{} claude-hook stop", dead_path("bin/winmux")));
        assert!(is_ymux_entry(&dead), "precondition: still recognised as ours");
        assert!(!ymux_entry_is_runnable(&[dead]));
    }

    #[test]
    fn a_rooted_posix_path_is_checked_even_on_windows() {
        // Path::is_absolute() is false for this shape on Windows, but a
        // settings.json written under WSL or copied off a Linux box carries
        // exactly it — hence the has_root() arm.
        let dead = entry("/ymux-nonexistent-hook-test/bin/winmux claude-hook stop");
        assert!(!ymux_entry_is_runnable(&[dead]));
    }

    #[test]
    fn an_absolute_path_that_exists_is_runnable() {
        // Any real file will do; the check is existence, not executability.
        let me = std::env::current_exe().expect("test binary path");
        let cmd = format!("{} claude-hook stop", me.display());
        assert!(ymux_entry_is_runnable(&[entry(&cmd)]));
    }

    #[test]
    fn unresolvable_commands_are_left_alone() {
        // A bare name resolves through PATH and a placeholder resolves at
        // run time. Neither can be checked here, and treating them as dead
        // would rewrite a working hook on every single run.
        assert!(ymux_entry_is_runnable(&[entry("ymux claude-hook stop")]));
        assert!(ymux_entry_is_runnable(&[entry("${YMUX_BIN} claude-hook stop")]));
        assert!(ymux_entry_is_runnable(&[entry("%YMUX_BIN% claude-hook stop")]));
    }

    #[test]
    fn a_quoted_path_with_spaces_is_read_whole() {
        // Splitting on the first space would test "C:\Program" and call a
        // perfectly good Windows install dead.
        let quoted = entry(&format!(
            "\"{}\" claude-hook stop",
            dead_path("with space/bin/ymux")
        ));
        assert!(!ymux_entry_is_runnable(&[quoted]));
        // The converse: a real path with a space must still read as live.
        let me = std::env::current_exe().expect("test binary path");
        let live = entry(&format!("\"{}\" claude-hook stop", me.display()));
        assert!(ymux_entry_is_runnable(&[live]));
    }

    /// The migration case end-to-end, as the bootstrap runs it on every
    /// connect (`setup-hooks --agent claude --source bundled`, no --force):
    /// a settings.json whose entries still point at the OLD binary must
    /// come back as `would_register`, not `already_present`. The old
    /// binary exists (the compat symlink keeps it alive), so the
    /// runnable check alone is satisfied — this is what
    /// `ymux_entry_points_at` adds.
    #[test]
    fn bootstrap_rerun_replaces_entries_on_the_old_binary() {
        let dir = std::env::temp_dir().join(format!(
            "ymux-hooks-migrate-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("tmp dir");
        let old_exe = dir.join("winmux");
        let new_exe = dir.join("ymux");
        fs::write(&old_exe, b"").expect("old exe");
        fs::write(&new_exe, b"").expect("new exe");
        let settings = dir.join("settings.json");
        let old_cmd = |ev: &str| format!("{} claude-hook {ev}", old_exe.display());
        fs::write(
            &settings,
            serde_json::to_string(&json!({
                "hooks": {
                    "Stop": [entry(&old_cmd("stop"))],
                    "PreToolUse": [entry(&old_cmd("pre-tool-use"))],
                    "UserPromptSubmit": [entry(&old_cmd("user-prompt-submit"))],
                },
                "ymux_meta": { "hooks_version": "1.5.0", "agent": "claude-code" }
            }))
            .expect("json"),
        )
        .expect("settings");

        let spec = bundled_claude_spec();
        let status = apply_to_settings(
            &dir,
            &settings,
            &new_exe.display().to_string(),
            &spec,
            true,
            false,
        );
        let AgentStatus::DryRun { would_register, already_present, .. } = status else {
            panic!("expected DryRun, got {status:?}");
        };
        assert!(already_present.is_empty(), "nothing on the old binary is 'present': {already_present:?}");
        for ev in ["Stop", "PreToolUse", "UserPromptSubmit"] {
            assert!(would_register.iter().any(|e| e == ev), "{ev} must be rewritten");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    // ── D17: events dropped from the spec ────────────────────────────

    fn theirs() -> Value {
        json!({ "type": "command", "command": "/usr/local/bin/lint --fast" })
    }

    // Pins D17(a): the install loop only visited spec events, so a ymux
    // entry under an event the spec dropped stayed for good.
    #[test]
    fn a_dropped_event_loses_our_hooks_and_nothing_else() {
        let spec = bundled_claude_spec();
        assert!(!spec.events.contains_key("PreCompact"), "precondition");
        assert!(!spec.events.contains_key("SubagentStop"), "precondition");
        let mut root = json!({
            // Ours alone, in the pre-rename spelling: the key goes.
            "PreCompact": [entry("/home/y/.winmux/bin/winmux claude-hook pre-compact")],
            // Ours beside a user's hook in ONE entry, plus a user's own entry.
            "SubagentStop": [
                {
                    "matcher": "",
                    "hooks": [
                        { "type": "command", "command": "/home/y/.ymux/bin/ymux claude-hook subagent-stop" },
                        theirs(),
                    ]
                },
                { "matcher": "", "hooks": [theirs()] },
            ],
            // A dropped event that is all the user's: untouched.
            "PostToolUse": [{ "matcher": "", "hooks": [theirs()] }],
            // Still in the spec: not this function's business.
            "Stop": [entry("/home/y/.winmux/bin/winmux claude-hook stop")],
        });
        let hooks = root.as_object_mut().expect("object");
        let mut removed = prune_dropped_events(hooks, &spec);
        removed.sort();
        assert_eq!(removed, vec!["PreCompact".to_string(), "SubagentStop".to_string()]);
        assert!(root.get("PreCompact").is_none(), "an emptied event key is removed");
        assert_eq!(
            root["SubagentStop"],
            json!([
                { "matcher": "", "hooks": [theirs()] },
                { "matcher": "", "hooks": [theirs()] },
            ])
        );
        assert_eq!(root["PostToolUse"], json!([{ "matcher": "", "hooks": [theirs()] }]));
        assert_eq!(root["Stop"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn a_spec_without_events_prunes_nothing() {
        // The empty fallback spec (or a fetched file with no `events`) must
        // not read as "every event was dropped".
        let spec = bundled_spec_for("codex");
        assert!(spec.events.is_empty(), "precondition");
        let mut root = json!({ "Stop": [entry("/home/y/.ymux/bin/ymux claude-hook stop")] });
        let before = root.clone();
        let removed = prune_dropped_events(root.as_object_mut().expect("object"), &spec);
        assert!(removed.is_empty());
        assert_eq!(root, before);
    }

    fn settings_dir(name: &str) -> (PathBuf, PathBuf, String) {
        let dir = std::env::temp_dir().join(format!("ymux-hooks-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("tmp dir");
        let exe = dir.join("ymux");
        fs::write(&exe, b"").expect("exe");
        (dir.clone(), dir.join("settings.json"), exe.display().to_string())
    }

    fn read_json(path: &std::path::Path) -> Value {
        serde_json::from_str(&fs::read_to_string(path).expect("read settings")).expect("json")
    }

    fn no_tmp_left(dir: &std::path::Path) -> bool {
        fs::read_dir(dir)
            .expect("read dir")
            .filter_map(Result::ok)
            .all(|e| !e.file_name().to_string_lossy().contains("ymux-tmp"))
    }

    // Pins D17(a) end to end: with every spec event already installed the
    // run took the "nothing to do" path and never looked at other events.
    #[test]
    fn a_rerun_removes_our_entry_under_a_dropped_event() {
        let (dir, settings, exe) = settings_dir("dropped-event");
        let spec = bundled_claude_spec();
        let first = apply_to_settings(&dir, &settings, &exe, &spec, false, false);
        assert!(matches!(first, AgentStatus::Done { unchanged: false, .. }), "got {first:?}");

        let mut root = read_json(&settings);
        root["hooks"]["PreCompact"] = json!([entry(&format!("{exe} claude-hook pre-compact"))]);
        root["hooks"]["PostToolUse"] = json!([{ "matcher": "", "hooks": [theirs()] }]);
        fs::write(&settings, serde_json::to_string(&root).expect("json")).expect("write");

        let dry = apply_to_settings(&dir, &settings, &exe, &spec, true, false);
        let AgentStatus::DryRun { would_register, would_remove, .. } = dry else {
            panic!("expected DryRun, got {dry:?}");
        };
        assert!(would_register.is_empty(), "got {would_register:?}");
        assert_eq!(would_remove, vec!["PreCompact".to_string()]);
        assert!(read_json(&settings)["hooks"].get("PreCompact").is_some(), "a dry run writes nothing");

        let second = apply_to_settings(&dir, &settings, &exe, &spec, false, false);
        let AgentStatus::Done { registered, removed, unchanged, .. } = second else {
            panic!("expected Done, got {second:?}");
        };
        assert!(!unchanged);
        assert!(registered.is_empty(), "got {registered:?}");
        assert_eq!(removed, vec!["PreCompact".to_string()]);
        let after = read_json(&settings);
        assert!(after["hooks"].get("PreCompact").is_none());
        assert_eq!(after["hooks"]["PostToolUse"], json!([{ "matcher": "", "hooks": [theirs()] }]));
        for ev in spec.events.keys() {
            assert_eq!(after["hooks"][ev].as_array().map(Vec::len), Some(1), "{ev}");
        }
        assert!(no_tmp_left(&dir));
        let _ = fs::remove_dir_all(&dir);
    }

    // Covers the D17(b) path: a stale stamp with nothing else to change is
    // rewritten through the tmp + rename writer. Atomicity itself is not
    // observable from a unit test; this pins the result and the cleanup.
    #[test]
    fn a_stamp_only_refresh_rewrites_the_version_and_leaves_no_tmp() {
        let (dir, settings, exe) = settings_dir("stamp-only");
        let spec = bundled_claude_spec();
        let _ = apply_to_settings(&dir, &settings, &exe, &spec, false, false);
        let mut root = read_json(&settings);
        root["ymux_meta"]["hooks_version"] = json!("0.0.1");
        root["theme"] = json!("dark");
        fs::write(&settings, serde_json::to_string(&root).expect("json")).expect("write");

        let status = apply_to_settings(&dir, &settings, &exe, &spec, false, false);
        assert!(matches!(status, AgentStatus::Done { unchanged: true, .. }), "got {status:?}");
        let after = read_json(&settings);
        assert_eq!(after["ymux_meta"]["hooks_version"], json!(spec.ymux_hooks_version));
        assert_eq!(after["theme"], json!("dark"), "the rest of the file survives");
        assert!(no_tmp_left(&dir));
        let _ = fs::remove_dir_all(&dir);
    }

    // This one fails on the old plain `fs::write`, which writes THROUGH a
    // symlink: tmp + rename replaces the link and leaves its target alone.
    #[cfg(unix)]
    #[test]
    fn a_stamp_only_refresh_renames_over_the_file_instead_of_writing_into_it() {
        let (dir, settings, exe) = settings_dir("stamp-rename");
        let spec = bundled_claude_spec();
        let _ = apply_to_settings(&dir, &settings, &exe, &spec, false, false);
        let mut root = read_json(&settings);
        root["ymux_meta"]["hooks_version"] = json!("0.0.1");
        let stale = serde_json::to_string(&root).expect("json");
        let target = dir.join("real-settings.json");
        fs::write(&target, &stale).expect("write target");
        fs::remove_file(&settings).expect("remove settings");
        std::os::unix::fs::symlink(&target, &settings).expect("symlink");

        let status = apply_to_settings(&dir, &settings, &exe, &spec, false, false);
        assert!(matches!(status, AgentStatus::Done { unchanged: true, .. }), "got {status:?}");
        let meta = fs::symlink_metadata(&settings).expect("metadata");
        assert!(!meta.file_type().is_symlink(), "the rename replaces the link");
        assert_eq!(fs::read_to_string(&target).expect("read target"), stale);
        assert_eq!(
            read_json(&settings)["ymux_meta"]["hooks_version"],
            json!(spec.ymux_hooks_version)
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn entry_pointing_at_another_binary_is_stale() {
        let exe = "/home/u/.ymux/bin/ymux";
        let old = vec![hook_entry("", "/home/u/.winmux/bin/winmux claude-hook stop", None)];
        assert!(!ymux_entry_points_at(&old, exe));
        let cur = vec![hook_entry("", "/home/u/.ymux/bin/ymux claude-hook stop", None)];
        assert!(ymux_entry_points_at(&cur, exe));
        // Windows quoting + backslashes normalise to the same thing.
        let win = vec![hook_entry("", "\"C:\\Users\\y\\ymux-cli.exe\" claude-hook stop", None)];
        assert!(ymux_entry_points_at(&win, "C:\\Users\\y\\ymux-cli.exe"));
    }

    #[test]
    fn entry_without_spec_timeout_is_stale() {
        let old = vec![hook_entry("Bash", "/home/u/.ymux/bin/ymux claude-hook pre-tool-use", None)];
        assert!(!ymux_entry_has_timeout(&old, Some(600)));
        let new = vec![hook_entry("Bash", "/home/u/.ymux/bin/ymux claude-hook pre-tool-use", Some(600))];
        assert!(ymux_entry_has_timeout(&new, Some(600)));
        assert!(ymux_entry_has_timeout(&old, None));
    }

    #[test]
    fn timeout_is_written_only_when_the_spec_has_one() {
        let with = hook_entry("Bash", "/home/u/.ymux/bin/ymux claude-hook pre-tool-use", Some(600));
        assert_eq!(with["hooks"][0]["timeout"], json!(600));
        assert!(is_ymux_entry(&with));
        let without = hook_entry("", "/home/u/.ymux/bin/ymux claude-hook stop", None);
        assert!(without["hooks"][0].get("timeout").is_none());
        // The bundled spec carries it on PreToolUse alone.
        let spec = bundled_claude_spec();
        assert_eq!(spec.events["PreToolUse"].timeout, Some(600));
        assert_eq!(spec.events["Stop"].timeout, None);
    }

    #[test]
    fn entries_that_are_not_ours_do_not_make_us_dead() {
        // Someone else's broken hook sitting in the same event array must
        // not drag our live entry into a rewrite.
        let me = std::env::current_exe().expect("test binary path");
        let ours = entry(&format!("{} claude-hook stop", me.display()));
        // The crate is named `ymux`, so the test binary path contains it —
        // without this the filter yields nothing and the assert is vacuous.
        assert!(is_ymux_entry(&ours), "precondition: recognised as ours");
        let theirs = entry(&format!("{} run", dead_path("other-tool/bin/thing")));
        assert!(!is_ymux_entry(&theirs), "precondition: not ours");
        assert!(ymux_entry_is_runnable(&[theirs, ours]));
    }
}
