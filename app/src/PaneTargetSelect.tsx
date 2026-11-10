// The pane picker of the Diff pane: where a prompt (line notes, or the
// "Fix broken checks" prompt) is pasted. One copy, used by both trays.

import { For } from "solid-js";
import { t } from "./i18n";
import type { DiffSendTarget } from "./diffNotes";

export function PaneTargetSelect(p: {
  targets: DiffSendTarget[];
  value: string;
  onChange: (paneId: string) => void;
}) {
  const valid = () => p.targets.some((x) => x.paneId === p.value);
  return (
    <label class="diff-notes-label">
      {t("diff.notes.target")}{" "}
      <select onChange={(e) => p.onChange(e.currentTarget.value)}>
        <option value="" selected={!valid()}>
          {p.targets.length > 0 ? t("diff.notes.choosePane") : t("diff.notes.noPanes")}
        </option>
        <For each={p.targets}>
          {(x) => (
            <option value={x.paneId} selected={x.paneId === p.value}>
              {x.isAgent ? `${x.label} (${t("diff.notes.agentTag")})` : x.label}
            </option>
          )}
        </For>
      </select>
    </label>
  );
}
