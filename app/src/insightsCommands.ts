// "Copy investigation commands" — the other half of handing data to Claude.
//
// The pasteable report answers the questions we thought to ask. This answers
// the ones we didn't: it hands over the PATHS, the schema, and a few working
// queries, so an assistant with shell access on the machine can slice the data
// itself instead of being limited to a summary we pre-chewed.
//
// There is no URL here on purpose. Neither store is exposed over HTTP outside
// 127.0.0.1, and none of this suggests changing that — these are local reads on
// a box the user already has a session on.
//
// Pure and DOM-free, like insightsReport, so it can be unit tested.

export type CommandTarget = "metrics" | "claude";

export interface CommandStrings {
  /** Already-translated lead-in explaining what the block is for. */
  intro: string;
  /** True when the workspace is the desktop itself rather than an SSH host. */
  local: boolean;
}

// Where each store actually lives. The metrics path is the daemon's data dir
// (`~/.ymux/server`, see cmd/ymux-server/main.go defBase); transcripts are
// Claude Code's own directory.
const METRICS_DB = "~/.ymux/server/metrics.db";
// local workspace: same schema, written by insights_store.rs
const METRICS_DB_LOCAL = "insights-local.db";
const METRICS_DB_LOCAL_NOTE = `# Run from the folder holding the DB:
#   Windows: %APPDATA%\\ymux\\insights-local.db
#   macOS:   ~/Library/Application Support/ymux/insights-local.db
`;
const TRANSCRIPTS = "~/.claude/projects";

const METRICS_SCHEMA = `samples(ts, cpu_pct, load1, mem_used, mem_total, swap_used, net_rx_bps, net_tx_bps)
disk_samples(ts, mount, used, total)
docker_samples(ts, cid, name, cpu_pct, mem_used, state)
-- ts is unix seconds, one row per 5s tick, swept to a 7-day window.`;

function metricsBlock(local: boolean): string {
  const db = local ? METRICS_DB_LOCAL : METRICS_DB;
  return `${local ? METRICS_DB_LOCAL_NOTE : ""}# Schema
${METRICS_SCHEMA}

# Busiest 20 minutes of the last day
sqlite3 ${db} "SELECT datetime(ts,'unixepoch','localtime') AS t, ROUND(cpu_pct,1) AS cpu, ROUND(mem_used*100.0/NULLIF(mem_total,0),1) AS mem_pct, load1 FROM samples WHERE ts > strftime('%s','now','-1 day') ORDER BY cpu_pct DESC LIMIT 20;"

# Hourly averages, so a spike is visible against its own baseline
sqlite3 ${db} "SELECT datetime(ts/3600*3600,'unixepoch','localtime') AS hour, COUNT(*) AS n, ROUND(AVG(cpu_pct),1) AS cpu_avg, ROUND(MAX(cpu_pct),1) AS cpu_max, ROUND(AVG(load1),2) AS load_avg FROM samples WHERE ts > strftime('%s','now','-2 days') GROUP BY hour ORDER BY hour;"

# Which mount is growing, and by how much
sqlite3 ${db} "SELECT mount, ROUND(MIN(used)/1073741824.0,2) AS gb_first, ROUND(MAX(used)/1073741824.0,2) AS gb_last, ROUND((MAX(used)-MIN(used))/1073741824.0,2) AS gb_growth FROM disk_samples WHERE ts > strftime('%s','now','-7 days') GROUP BY mount ORDER BY gb_growth DESC;"

# Containers that kept dying: uptime as a share of their samples
sqlite3 ${db} "SELECT name, COUNT(*) AS n, ROUND(100.0*SUM(state='running')/COUNT(*),1) AS uptime_pct, ROUND(AVG(cpu_pct),1) AS cpu_avg FROM docker_samples WHERE ts > strftime('%s','now','-1 day') GROUP BY name ORDER BY uptime_pct ASC;"`;
}

function claudeBlock(local: boolean): string {
  const root = local
    ? "$HOME/.claude/projects   (on Windows: %USERPROFILE%\\.claude\\projects)"
    : TRANSCRIPTS;
  return `# Where the data is
${root}
One JSON object per line, one file per session, one directory per project.
Assistant lines carry:
  .timestamp                              RFC3339
  .sessionId  .cwd  .isSidechain          who / where / subagent?
  .message.model                          e.g. claude-opus-5
  .message.usage.input_tokens
  .message.usage.output_tokens
  .message.usage.cache_read_input_tokens
  .message.usage.cache_creation.ephemeral_5m_input_tokens
  .message.usage.cache_creation.ephemeral_1h_input_tokens
  .message.usage.speed                    standard | fast
There is NO cost field — cost is tokens x per-model price, computed by the caller.

# Tokens per model, last 7 days (needs jq)
find ${TRANSCRIPTS} -name '*.jsonl' -mtime -7 -exec cat {} + \\
 | jq -rs 'map(select(.type=="assistant" and .message.usage)) | group_by(.message.model)[]
     | {model: .[0].message.model, calls: length,
        in: (map(.message.usage.input_tokens // 0) | add),
        out: (map(.message.usage.output_tokens // 0) | add),
        cache_read: (map(.message.usage.cache_read_input_tokens // 0) | add)}'

# Tokens per project
find ${TRANSCRIPTS} -name '*.jsonl' -mtime -7 -exec cat {} + \\
 | jq -rs 'map(select(.type=="assistant" and .message.usage)) | group_by(.cwd)[]
     | {project: .[0].cwd, calls: length,
        out: (map(.message.usage.output_tokens // 0) | add)}'

# How much came from subagents rather than the main loop
find ${TRANSCRIPTS} -name '*.jsonl' -mtime -7 -exec cat {} + \\
 | jq -rs 'map(select(.type=="assistant" and .message.usage))
     | {total: length, subagent: (map(select(.isSidechain)) | length)}'

# The single most expensive session by output tokens
find ${TRANSCRIPTS} -name '*.jsonl' -mtime -30 -exec cat {} + \\
 | jq -rs 'map(select(.type=="assistant" and .message.usage)) | group_by(.sessionId)
     | map({session: .[0].sessionId, cwd: .[0].cwd,
            out: (map(.message.usage.output_tokens // 0) | add)})
     | sort_by(-.out) | .[0:5][]'`;
}

/**
 * Build the pasteable command block for one tab.
 *
 * The caller supplies the intro in the user's language; the commands
 * themselves stay English, because they are shell, not prose.
 */
export function buildCommands(target: CommandTarget, s: CommandStrings): string {
  const body = target === "metrics" ? metricsBlock(s.local) : claudeBlock(s.local);
  return [
    s.intro,
    "",
    body,
    "",
    "# These are read-only queries against files that already exist on this",
    "# machine. Nothing here is exposed over the network.",
    "",
  ].join("\n");
}
