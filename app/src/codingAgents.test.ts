// cd app && node --experimental-strip-types --test src/codingAgents.test.ts
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  connectCommands,
  effectiveAgents,
  effectiveAgentsForWorkspace,
  grokResumeCommand,
  isMachineHeader,
  type AgentTreeNode,
  type CodingAgentsSettings,
} from "./codingAgents.ts";

const both: CodingAgentsSettings = { claude: true, grok: true, default: "claude" };

const row = (
  id: string,
  parent: string | null,
  extra: Partial<AgentTreeNode> = {},
): AgentTreeNode => ({
  id,
  parent_id: parent,
  is_project_root: false,
  is_folder: false,
  sort_order: null,
  ...extra,
});

test("a missing config is Claude only, on one server and on many", () => {
  assert.deepEqual(effectiveAgents(undefined, false), ["claude"]);
  assert.deepEqual(effectiveAgents(undefined, true), ["claude"]);
});

test("one server offers only the default agent", () => {
  assert.deepEqual(effectiveAgents(both, false), ["claude"]);
  assert.deepEqual(effectiveAgents({ ...both, default: "grok" }, false), ["grok"]);
});

test("an unchecked default falls back to the agent that is on", () => {
  assert.deepEqual(
    effectiveAgents({ claude: false, grok: true, default: "claude" }, false),
    ["grok"],
  );
  assert.deepEqual(
    effectiveAgents({ claude: true, grok: false, default: "grok" }, false),
    ["claude"],
  );
});

test("a multi-agent server offers every checked agent", () => {
  assert.deepEqual(effectiveAgents(both, true), ["claude", "grok"]);
  assert.deepEqual(effectiveAgents({ ...both, claude: false }, true), ["grok"]);
});

test("no checked agent offers no agent", () => {
  assert.deepEqual(
    effectiveAgents({ claude: false, grok: false, default: "claude" }, true),
    [],
  );
});

test("the command menu follows the effective list", () => {
  assert.deepEqual(connectCommands(["claude"]), [
    "plain",
    "claude",
    "claude-continue",
    "claude-resume",
    "claude-skip",
    "from-list",
    "custom",
  ]);
  assert.deepEqual(connectCommands(["grok"]), ["plain", "grok", "grok-c", "grok-resume", "custom"]);
  assert.deepEqual(connectCommands(["claude", "grok"]), [
    "plain",
    "claude",
    "claude-continue",
    "claude-resume",
    "claude-skip",
    "from-list",
    "grok",
    "grok-c",
    "grok-resume",
    "custom",
  ]);
  assert.deepEqual(connectCommands([]), ["plain", "from-list", "custom"]);
});

test("a grok resume id is one shell token", () => {
  assert.equal(grokResumeCommand("abc_1"), "grok --resume abc_1");
  assert.equal(grokResumeCommand("Abc-9"), "grok --resume Abc-9");
  assert.equal(grokResumeCommand(""), null);
  assert.equal(grokResumeCommand("-x"), null);
  assert.equal(grokResumeCommand("abc;rm"), null);
  assert.equal(grokResumeCommand("a b"), null);
});

test("a screen reads multi_agent from the machine, not from a folder", () => {
  const all = [
    row("srv", null, { multi_agent: true }),
    row("app", "srv", { is_folder: true, is_project_root: true }),
    row("shell", "app"),
  ];
  assert.equal(isMachineHeader(all[0]), true);
  assert.equal(isMachineHeader(all[1]), false);
  assert.deepEqual(
    effectiveAgentsForWorkspace(both, all, "shell"),
    ["claude", "grok"],
  );
  assert.deepEqual(
    effectiveAgentsForWorkspace(both, [row("srv", null), row("shell", "srv")], "shell"),
    ["claude"],
  );
});

test("a chain with no machine offers only the default agent", () => {
  const all = [row("app", null, { is_project_root: true }), row("shell", "app")];
  assert.deepEqual(effectiveAgentsForWorkspace(both, all, "shell"), ["claude"]);
});
