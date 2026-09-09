// Phase 84.F: throwaway harness for the pane tab strip's CSS.
//
// NOT part of the app. Rule #17 bans building the Tauri binary here, and
// the strip's problem was never Rust — it was 136 lines of App.css that a
// merge deleted. This mounts the REAL <PaneTabs> with the REAL App.css so
// the layout can be seen in a browser without a cargo build. Delete once
// the strip has been confirmed in a real run.
import { createSignal } from "solid-js";
import { render } from "solid-js/web";
import "./App.css";
import { PaneTabs } from "./PaneTabs";
import type { LayoutNode } from "./types";

type PaneNode = Extract<LayoutNode, { kind: "pane" }>;

const pane = (id: string, title: string, emoji: string | null): PaneNode => ({
  kind: "pane",
  pane_id: id,
  pane_kind: "terminal",
  connection: null,
  browser: null,
  title,
  auto_title: null,
  annotation: null,
  color: null,
  emoji,
  help_topic: null,
  diff_source: null,
  smart_bidi: null,
  diff_cwd: null,
});

function Harness() {
  const [panes, setPanes] = createSignal<PaneNode[]>([
    pane("p1", "zsh — ~/proj", "🚀"),
    pane("p2", "claude code", null),
    pane("p3", "npm run dev", "📦"),
    pane("p4", "ssh srv-01", null),
  ]);
  const [active, setActive] = createSignal<string | null>("p2");
  let n = 5;

  return (
    <div style="height:100vh;display:flex;flex-direction:column;font-family:system-ui,sans-serif">
      <div style="padding:8px 14px;color:#7d8590;font-size:12px;background:#161b22;border-bottom:1px solid #21262d">
        workspace header (stand-in)
      </div>
      <PaneTabs
        panes={panes()}
        activePaneId={active()}
        connectedPaneIds={new Set(["p1", "p3"])}
        waitingPaneIds={new Set(["p4"])}
        notifiedPaneIds={new Set(["p3"])}
        panePulseEnabled={true}
        workspaceName="demo"
        workspaceColor="#7aa2f7"
        workspaceEmoji="🧪"
        agentLights={{ p2: "green", p4: "red" }}
        agentStateSince={{ p2: Date.now() - 42_000, p4: Date.now() - 5_000 }}
        agentNowMs={Date.now()}
        onSelect={setActive}
        onClose={(id) => setPanes((ps) => ps.filter((p) => p.pane_id !== id))}
        onNew={() => {
          const id = `p${n++}`;
          setPanes((ps) => [...ps, pane(id, `new tab ${id}`, null)]);
          setActive(id);
        }}
      />
      <div style="flex:1;display:flex;align-items:center;justify-content:center;background:#0e1116;color:#e6edf3;font-size:20px">
        active pane: {active() ?? "—"}
      </div>
    </div>
  );
}

render(() => <Harness />, document.getElementById("root") as HTMLElement);
