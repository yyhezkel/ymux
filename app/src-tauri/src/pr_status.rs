//! PR status for a Diff pane: `gh pr view` on the workspace's host.
//!
//! `gh` runs where the repo is: locally with an arg array, on WSL and SSH as
//! one `sh` script over the existing transport. The desktop never holds a
//! GitHub token, because `gh` uses the host's own login.
//!
//! Each failure that the user can fix outside the app (no `gh`, not logged
//! in, no PR, no GitHub remote, timeout) is a distinct `Ok` state, not an
//! `Err`. The frontend shows each one quietly.
//!
//! Rule #1: the gh output is repository content. Log the state kind, the
//! exit code and the check count only. Never log the text.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::diff_pane::lookup_pane_context;
use crate::local_setup;
use crate::worktrees::pick_ssh_handle_for_host;
use crate::{log_debug, AppState, Connection};
use ymux_core::shell_quote;
use ymux_core::ssh_exec::ExecOpts;

/// Limit for one `gh pr view` run, local or remote.
const GH_TIMEOUT_SECS: u64 = 20;
/// Limit on the checks sent to the UI. A rollup is rarely over 100.
const MAX_CHECKS: usize = 300;
/// Limit on one text field from gh, in chars.
const MAX_FIELD_CHARS: usize = 500;

const GH_ARGS: &[&str] = &[
    "pr",
    "view",
    "--json",
    "number,title,state,isDraft,url,headRefName,statusCheckRollup",
];

/// One entry of `statusCheckRollup`. GitHub returns two shapes: a
/// `CheckRun` (name, status, conclusion, detailsUrl) and a `StatusContext`
/// (context, state, targetUrl). Both pass through as they are; the frontend
/// classifies them (`prChecks.ts`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PrCheck {
    #[serde(rename = "__typename", default)]
    pub typename: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub workflow_name: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub details_url: Option<String>,
    #[serde(default)]
    pub target_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PrInfo {
    pub number: u64,
    #[serde(default)]
    pub title: String,
    /// OPEN, CLOSED or MERGED.
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub is_draft: bool,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub head_ref_name: String,
    #[serde(default)]
    pub status_check_rollup: Vec<PrCheck>,
}

/// What the UI shows. Hand-mirrored in `app/src/prChecks.ts` (`PrStatus`).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum PrStatus {
    Ok { pr: PrInfo },
    GhMissing,
    NotAuthenticated,
    NoPr,
    /// Not a git repository, or no remote points to a GitHub host.
    NotGithub,
    Timeout,
    /// No live SSH session to the host. The Diff pane shows the same case.
    NoSession,
    /// Anything else. `message` is gh's first line, for the UI only.
    Error { message: String },
}

impl PrStatus {
    fn kind(&self) -> &'static str {
        match self {
            PrStatus::Ok { .. } => "ok",
            PrStatus::GhMissing => "gh_missing",
            PrStatus::NotAuthenticated => "not_authenticated",
            PrStatus::NoPr => "no_pr",
            PrStatus::NotGithub => "not_github",
            PrStatus::Timeout => "timeout",
            PrStatus::NoSession => "no_session",
            PrStatus::Error { .. } => "error",
        }
    }
}

/// The result of one gh run, before classification.
#[derive(Debug, PartialEq)]
pub(crate) enum GhRun {
    /// The process ended. `text` holds stdout and stderr.
    Exited { code: i32, text: String },
    /// The local spawn found no `gh` binary.
    NotFound,
    TimedOut,
}

// ─── pure helpers (unit-tested) ──────────────────────────────────────

fn cap(s: &str) -> String {
    if s.chars().count() <= MAX_FIELD_CHARS {
        return s.to_string();
    }
    s.chars().take(MAX_FIELD_CHARS).collect()
}

fn cap_opt(s: &mut Option<String>) {
    if let Some(v) = s.as_mut() {
        *v = cap(v);
    }
}

/// gh writes the JSON object on stdout. On the remote transports stderr is
/// merged into the same text, so start at the first `{` line.
pub(crate) fn parse_pr_json(text: &str) -> Result<PrInfo, String> {
    let mut at = 0usize;
    let mut start = None;
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with('{') {
            start = Some(at);
            break;
        }
        at += line.len();
    }
    let start = start.ok_or_else(|| "gh returned no JSON".to_string())?;
    let mut pr: PrInfo = serde_json::Deserializer::from_str(&text[start..])
        .into_iter::<PrInfo>()
        .next()
        .ok_or_else(|| "gh returned no JSON".to_string())?
        .map_err(|e| format!("gh returned JSON this version cannot read: {e}"))?;
    pr.title = cap(&pr.title);
    pr.url = cap(&pr.url);
    pr.head_ref_name = cap(&pr.head_ref_name);
    pr.status_check_rollup.truncate(MAX_CHECKS);
    for c in &mut pr.status_check_rollup {
        for f in [
            &mut c.typename,
            &mut c.name,
            &mut c.context,
            &mut c.workflow_name,
            &mut c.status,
            &mut c.conclusion,
            &mut c.state,
            &mut c.details_url,
            &mut c.target_url,
        ] {
            cap_opt(f);
        }
    }
    Ok(pr)
}

fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("gh failed");
    cap(line)
}

/// Map one gh run to a UI state. gh's exit codes: 0 success, 1 failure,
/// 2 cancelled, 4 authentication required. A shell gives 127 when the
/// command is not found. gh prints English only, so the text match is safe.
pub(crate) fn classify(run: GhRun) -> PrStatus {
    let (code, text) = match run {
        GhRun::NotFound => return PrStatus::GhMissing,
        GhRun::TimedOut => return PrStatus::Timeout,
        GhRun::Exited { code, text } => (code, text),
    };
    if code == 0 {
        return match parse_pr_json(&text) {
            Ok(pr) => PrStatus::Ok { pr },
            Err(message) => PrStatus::Error { message },
        };
    }
    if code == 127 {
        return PrStatus::GhMissing;
    }
    let lower = text.to_ascii_lowercase();
    if code == 4 || lower.contains("gh auth login") {
        return PrStatus::NotAuthenticated;
    }
    if lower.contains("no pull requests found") {
        return PrStatus::NoPr;
    }
    if lower.contains("not a git repository")
        || lower.contains("none of the git remotes")
        || lower.contains("no git remotes found")
    {
        return PrStatus::NotGithub;
    }
    PrStatus::Error { message: first_line(&text) }
}

/// The `sh` script for the WSL and SSH transports. Every interpolated value
/// is shell-quoted. The PATH additions are fixed strings: a non-login exec
/// channel often lacks `~/.local/bin` and Homebrew, where `gh` can live.
/// `GH_NO_UPDATE_NOTIFIER` keeps the update notice out of the merged JSON.
pub(crate) fn gh_script(cwd: &str) -> String {
    let mut s = format!(
        "PATH=\"$PATH:$HOME/.local/bin:$HOME/bin:/usr/local/bin:/home/linuxbrew/.linuxbrew/bin\"; \
         cd {} || exit 1; \
         command -v gh >/dev/null 2>&1 || exit 127; \
         GH_NO_UPDATE_NOTIFIER=1 GH_PROMPT_DISABLED=1 NO_COLOR=1 gh",
        shell_quote(cwd)
    );
    for a in GH_ARGS {
        s.push(' ');
        s.push_str(&shell_quote(a));
    }
    s.push_str(" 2>&1");
    s
}

// ─── transports ──────────────────────────────────────────────────────

async fn run_gh_local(cwd: &str) -> GhRun {
    let mut cmd = tokio::process::Command::new("gh");
    cmd.args(GH_ARGS)
        .current_dir(cwd)
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("GH_PROMPT_DISABLED", "1")
        .env("NO_COLOR", "1")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    // A Finder-launched app lacks Homebrew on PATH, where gh usually is.
    #[cfg(not(windows))]
    cmd.env("PATH", local_setup::unix_path_env());
    // Do not flash a console window out of the GUI process.
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let fut = cmd.output();
    match tokio::time::timeout(Duration::from_secs(GH_TIMEOUT_SECS), fut).await {
        Err(_) => GhRun::TimedOut,
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => GhRun::NotFound,
        Ok(Err(e)) => GhRun::Exited { code: -1, text: format!("spawn gh: {e}") },
        Ok(Ok(out)) => {
            let mut text = String::from_utf8_lossy(&out.stdout).to_string();
            if out.status.code() != Some(0) {
                text.push('\n');
                text.push_str(&String::from_utf8_lossy(&out.stderr));
            }
            GhRun::Exited { code: out.status.code().unwrap_or(-1), text }
        }
    }
}

/// `Err` means no live SSH session, which is its own UI state.
async fn run_gh_over(state: &AppState, conn: &Option<Connection>, cwd: &str) -> Result<GhRun, ()> {
    match conn {
        None | Some(Connection::Local { .. }) => Ok(run_gh_local(cwd).await),
        Some(Connection::Wsl { distro }) => {
            let script = gh_script(cwd);
            let fut = local_setup::wsl_exec(distro.as_deref(), None, &script);
            Ok(match tokio::time::timeout(Duration::from_secs(GH_TIMEOUT_SECS), fut).await {
                Err(_) => GhRun::TimedOut,
                Ok(Err(e)) => GhRun::Exited { code: -1, text: e },
                Ok(Ok((code, text))) => GhRun::Exited { code, text },
            })
        }
        Some(Connection::Ssh { host, user, port, .. }) => {
            let handle = pick_ssh_handle_for_host(state, host, user, *port).ok_or(())?;
            // `sh -c` so a non-POSIX login shell (fish) cannot choke on it.
            let cmd = format!("sh -c {}", shell_quote(&gh_script(cwd)));
            let out = ymux_core::ssh_exec::exec(&handle, &cmd, ExecOpts::timeout_secs(GH_TIMEOUT_SECS)).await;
            Ok(match out {
                Err(e) => GhRun::Exited { code: -1, text: e },
                Ok(o) if o.timed_out => GhRun::TimedOut,
                Ok(o) => match o.require_code() {
                    Ok(code) => GhRun::Exited { code, text: o.stdout },
                    Err(e) => GhRun::Exited { code: -1, text: e },
                },
            })
        }
    }
}

// ─── command ─────────────────────────────────────────────────────────

/// The PR of the branch checked out in a Diff pane's cwd, with its checks.
/// The cwd is the pane's own (the worktree strip can point it elsewhere).
#[tauri::command]
pub(crate) async fn diff_pane_pr_status(
    state: State<'_, AppState>,
    pane_id: String,
) -> Result<PrStatus, String> {
    let ctx = lookup_pane_context(&state, &pane_id)?
        .ok_or_else(|| format!("no Diff pane with id {pane_id}"))?;
    let cwd = ctx
        .cwd
        .clone()
        .filter(|c| !c.trim().is_empty())
        .ok_or_else(|| "this workspace has no project directory".to_string())?;
    let (status, code) = match run_gh_over(&state, &ctx.conn, &cwd).await {
        Err(()) => (PrStatus::NoSession, None),
        Ok(run) => {
            let code = match &run {
                GhRun::Exited { code, .. } => Some(*code),
                _ => None,
            };
            (classify(run), code)
        }
    };
    // Rule #1: metadata only. Debug level, because this runs every minute.
    let checks = match &status {
        PrStatus::Ok { pr } => pr.status_check_rollup.len(),
        _ => 0,
    };
    log_debug(
        "PR",
        &format!(
            "pane={pane_id} kind={} exit={} checks={checks}",
            status.kind(),
            code.map_or_else(|| "-".to_string(), |c| c.to_string())
        ),
    );
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"headRefName":"feat/x","isDraft":false,"number":42,"state":"OPEN","statusCheckRollup":[{"__typename":"CheckRun","completedAt":"2026-11-10T10:00:00Z","conclusion":"FAILURE","detailsUrl":"https://github.com/o/r/actions/runs/123/job/456","name":"rust","startedAt":"2026-11-10T09:00:00Z","status":"COMPLETED","workflowName":"ci-windows"},{"__typename":"StatusContext","context":"ci/external","startedAt":"2026-11-10T09:00:00Z","state":"PENDING","targetUrl":"https://ci.example/1"}],"title":"Add X","url":"https://github.com/o/r/pull/42"}"#;

    #[test]
    fn parses_both_rollup_shapes() {
        let pr = parse_pr_json(SAMPLE).expect("parse");
        assert_eq!(pr.number, 42);
        assert_eq!(pr.state, "OPEN");
        assert_eq!(pr.head_ref_name, "feat/x");
        assert_eq!(pr.status_check_rollup.len(), 2);
        let run = &pr.status_check_rollup[0];
        assert_eq!(run.typename.as_deref(), Some("CheckRun"));
        assert_eq!(run.name.as_deref(), Some("rust"));
        assert_eq!(run.conclusion.as_deref(), Some("FAILURE"));
        assert_eq!(run.workflow_name.as_deref(), Some("ci-windows"));
        let ctx = &pr.status_check_rollup[1];
        assert_eq!(ctx.context.as_deref(), Some("ci/external"));
        assert_eq!(ctx.state.as_deref(), Some("PENDING"));
        assert_eq!(ctx.target_url.as_deref(), Some("https://ci.example/1"));
    }

    #[test]
    fn skips_noise_before_the_json() {
        let text = format!("some profile banner\n{SAMPLE}\n");
        assert_eq!(parse_pr_json(&text).expect("parse").number, 42);
    }

    #[test]
    fn caps_long_fields_and_check_count() {
        let many: Vec<String> = (0..MAX_CHECKS + 5)
            .map(|i| format!(r#"{{"__typename":"CheckRun","name":"c{i}"}}"#))
            .collect();
        let title = "t".repeat(MAX_FIELD_CHARS + 50);
        let text = format!(
            r#"{{"number":1,"title":"{title}","statusCheckRollup":[{}]}}"#,
            many.join(",")
        );
        let pr = parse_pr_json(&text).expect("parse");
        assert_eq!(pr.title.chars().count(), MAX_FIELD_CHARS);
        assert_eq!(pr.status_check_rollup.len(), MAX_CHECKS);
    }

    #[test]
    fn classifies_each_failure_distinctly() {
        let ex = |code: i32, text: &str| GhRun::Exited { code, text: text.to_string() };
        assert_eq!(classify(GhRun::NotFound), PrStatus::GhMissing);
        assert_eq!(classify(GhRun::TimedOut), PrStatus::Timeout);
        assert_eq!(classify(ex(127, "sh: gh: not found")), PrStatus::GhMissing);
        assert_eq!(
            classify(ex(4, "To get started with GitHub CLI, please run:  gh auth login")),
            PrStatus::NotAuthenticated
        );
        assert_eq!(
            classify(ex(1, "no pull requests found for branch \"main\"")),
            PrStatus::NoPr
        );
        assert_eq!(
            classify(ex(1, "fatal: not a git repository (or any of the parent directories): .git")),
            PrStatus::NotGithub
        );
        assert_eq!(
            classify(ex(1, "none of the git remotes configured for this repository point to a known GitHub host.")),
            PrStatus::NotGithub
        );
        assert_eq!(
            classify(ex(1, "\nHTTP 502: Bad Gateway\nmore")),
            PrStatus::Error { message: "HTTP 502: Bad Gateway".to_string() }
        );
        assert!(matches!(classify(ex(0, SAMPLE)), PrStatus::Ok { .. }));
        assert!(matches!(classify(ex(0, "not json")), PrStatus::Error { .. }));
    }

    #[test]
    fn script_quotes_the_cwd() {
        let s = gh_script("/srv/it's here");
        assert!(s.contains(&shell_quote("/srv/it's here")));
        assert!(s.contains("|| exit 127"));
        assert!(s.ends_with(" 2>&1"));
        assert!(s.contains("GH_NO_UPDATE_NOTIFIER=1"));
    }

    #[test]
    fn serializes_with_a_kind_tag() {
        let v = serde_json::to_value(PrStatus::NoPr).expect("json");
        assert_eq!(v, serde_json::json!({ "kind": "no_pr" }));
        let v = serde_json::to_value(PrStatus::Error { message: "x".into() }).expect("json");
        assert_eq!(v, serde_json::json!({ "kind": "error", "message": "x" }));
    }
}
