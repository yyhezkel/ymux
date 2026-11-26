// Which agents the connect wizard offers. The marks are manual: nothing
// probes the server for a `grok` or `claude` binary. A machine with
// `multi_agent` off offers only the default agent; a screen reads that
// flag from its machine ancestor, never from a folder in between.

import { ancestorsOf, isHeader, type TreeNode } from "./wsTree.ts";

export type AgentId = "claude" | "grok";

export interface CodingAgentsSettings {
  claude: boolean;
  grok: boolean;
  /** "claude" or "grok". Any other string is read as claude. */
  default: string;
}

export const DEFAULT_CODING_AGENTS: CodingAgentsSettings = {
  claude: true,
  grok: false,
  default: "claude",
};

/** Checked agents, Claude first. Missing config is the default install:
 *  Claude on, Grok off. `claude: false` is a real off, not "absent". */
export function enabledAgents(cfg: CodingAgentsSettings | undefined): AgentId[] {
  const out: AgentId[] = [];
  if (cfg?.claude !== false) out.push("claude");
  if (cfg?.grok === true) out.push("grok");
  return out;
}

/** What one server's connect wizard may launch. `multi` false keeps a
 *  single agent: the configured default when it is checked, otherwise the
 *  one agent that is. */
export function effectiveAgents(
  cfg: CodingAgentsSettings | undefined,
  multi: boolean,
): AgentId[] {
  const enabled = enabledAgents(cfg);
  if (enabled.length === 0 || multi) return enabled;
  const want: AgentId = cfg?.default === "grok" ? "grok" : "claude";
  if (enabled.includes(want)) return [want];
  return [enabled[0]];
}

export type ConnectCommand =
  | "plain"
  | "claude"
  | "claude-continue"
  | "claude-resume"
  | "claude-skip"
  | "from-list"
  | "grok"
  | "grok-c"
  | "grok-resume"
  | "custom";

/** One token we will place after `grok --resume` in a shell command. */
const GROK_SESSION_ID = /^[A-Za-z0-9][A-Za-z0-9_-]{0,80}$/;

/** `grok --resume <id>` when the id is a single safe token. Otherwise null. */
export function grokResumeCommand(id: string): string | null {
  if (!GROK_SESSION_ID.test(id)) return null;
  return `grok --resume ${id}`;
}

/** Command menu for the wizard. Shell, custom, and (when Claude is not
 *  the one agent on offer) the session list stay. Grok adds a fresh
 *  launch, continue-latest, and resume-from-its-own-list. */
export function connectCommands(agents: readonly AgentId[]): ConnectCommand[] {
  const claude = agents.includes("claude");
  const grok = agents.includes("grok");
  const out: ConnectCommand[] = ["plain"];
  if (claude) {
    out.push("claude", "claude-continue", "claude-resume", "claude-skip", "from-list");
  }
  if (grok) out.push("grok", "grok-c", "grok-resume");
  if (!claude && !grok) out.push("from-list");
  out.push("custom");
  return out;
}

export interface AgentTreeNode extends TreeNode {
  multi_agent?: boolean;
}

/** A header that is the machine itself: not a pinned folder, not a
 *  project root. Those two are headers too, and the flag does not live
 *  on them. */
export function isMachineHeader(
  w: Pick<TreeNode, "parent_id" | "is_project_root" | "is_folder">,
): boolean {
  return isHeader(w) && !w.is_folder && !w.is_project_root;
}

/** The machine above `workspaceId`, or the row itself when it is one.
 *  Undefined when the chain has no machine (treat multi_agent as off). */
export function machineOf<T extends AgentTreeNode>(
  all: readonly T[],
  workspaceId: string,
): T | undefined {
  const self = all.find((w) => w.id === workspaceId);
  if (!self) return undefined;
  return [self, ...ancestorsOf(all, workspaceId)].find((w) => isMachineHeader(w));
}

export function effectiveAgentsForWorkspace(
  cfg: CodingAgentsSettings | undefined,
  all: readonly AgentTreeNode[],
  workspaceId: string,
): AgentId[] {
  return effectiveAgents(cfg, machineOf(all, workspaceId)?.multi_agent === true);
}
