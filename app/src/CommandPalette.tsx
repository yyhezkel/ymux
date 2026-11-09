import { createMemo, createSignal, createEffect, createUniqueId, For, Show, onCleanup } from "solid-js";
import { t } from "./i18n";
import { matchesQuery } from "./paletteMatch";

// Phase 35 (#1.3): VSCode-style command palette. Opened with
// Ctrl+Shift+P. The command list is supplied by the parent (App.tsx)
// so each command calls the SAME handler the existing UI already uses
// — the palette is a second entry point, not a reimplementation.

export interface Command {
  id: string;
  label: string;
  // English label, so an English query works in a translated UI.
  labelEn?: string;
  // Current accelerator as display text. Absent when the command has no
  // binding or the user's binding does not parse.
  shortcut?: string | null;
  handler: () => void;
  // When present and returns false, the command is hidden (scope
  // constraint, e.g. pane commands with no active pane).
  enabled?: () => boolean;
  // Jump rows only. `search` replaces the default search fields, so a pane
  // id (hex) cannot match a short query. `status` is real text, so a screen
  // reader says it; the icon is hidden from readers.
  search?: readonly string[];
  status?: { icon: string; text: string };
  detail?: string;
  path?: string;
}

interface Props {
  open: boolean;
  commands: Command[];
  placeholder?: string;
  emptyText?: string;
  onClose: () => void;
}

export function CommandPalette(p: Props) {
  const [query, setQuery] = createSignal("");
  const [selected, setSelected] = createSignal(0);
  let inputRef: HTMLInputElement | undefined;
  const listId = createUniqueId();
  const optionId = (i: number) => `${listId}-${i}`;
  const placeholder = () => p.placeholder ?? t("cmd.palette.placeholder");

  const visible = createMemo(() =>
    p.commands.filter((c) => (c.enabled ? c.enabled() : true)),
  );

  const filtered = createMemo(() =>
    visible().filter((c) => matchesQuery(query(), c.search ?? [c.label, c.labelEn ?? "", c.id])),
  );

  // Reset query + selection and focus the input each time it opens.
  createEffect(() => {
    if (p.open) {
      setQuery("");
      setSelected(0);
      queueMicrotask(() => inputRef?.focus());
    }
  });

  // Keep the selection in range as the filter narrows.
  createEffect(() => {
    const n = filtered().length;
    if (selected() >= n) setSelected(n === 0 ? 0 : n - 1);
  });

  // Keep the keyboard selection visible in a long list.
  createEffect(() => {
    if (!p.open || filtered().length === 0) return;
    document.getElementById(optionId(selected()))?.scrollIntoView({ block: "nearest" });
  });

  const run = (cmd: Command | undefined) => {
    if (!cmd) return;
    p.onClose();
    // Defer so the modal is gone before the handler (which may open
    // another modal) fires.
    queueMicrotask(() => cmd.handler());
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      p.onClose();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      const n = filtered().length;
      if (n > 0) setSelected((i) => (i + 1) % n);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      const n = filtered().length;
      if (n > 0) setSelected((i) => (i - 1 + n) % n);
    } else if (e.key === "Enter") {
      e.preventDefault();
      run(filtered()[selected()]);
    }
  };

  // Global capture so Ctrl+Shift+P-opened palette grabs keys even
  // though the modal isn't a routed focus trap.
  createEffect(() => {
    if (!p.open) return;
    const handler = (e: KeyboardEvent) => onKeyDown(e);
    window.addEventListener("keydown", handler, true);
    onCleanup(() => window.removeEventListener("keydown", handler, true));
  });

  return (
    <Show when={p.open}>
      <div class="modal-backdrop" onClick={p.onClose}>
        <div
          class="command-palette"
          role="dialog"
          aria-modal="true"
          aria-label={placeholder()}
          onClick={(e) => e.stopPropagation()}
        >
          {/* Focus stays in the input, so the combobox pattern is what makes
              a reader speak the selected row and its status. */}
          <input
            ref={inputRef}
            class="command-palette-input"
            type="text"
            role="combobox"
            aria-autocomplete="list"
            aria-label={placeholder()}
            aria-expanded={filtered().length > 0 ? "true" : "false"}
            aria-controls={filtered().length > 0 ? listId : undefined}
            aria-activedescendant={filtered().length > 0 ? optionId(selected()) : undefined}
            placeholder={placeholder()}
            value={query()}
            onInput={(e) => {
              setQuery(e.currentTarget.value);
              setSelected(0);
            }}
          />
          <div class="command-palette-list">
            <Show
              when={filtered().length > 0}
              fallback={
                <div class="command-palette-empty" role="status">
                  {p.emptyText ?? t("cmd.palette.empty")}
                </div>
              }
            >
              <div id={listId} role="listbox" aria-label={placeholder()}>
                <For each={filtered()}>
                  {(cmd, i) => (
                    <div
                      id={optionId(i())}
                      role="option"
                      aria-selected={i() === selected() ? "true" : "false"}
                      class={`command-palette-item ${i() === selected() ? "selected" : ""}`}
                      onMouseEnter={() => setSelected(i())}
                      onClick={() => run(cmd)}
                    >
                      <Show when={cmd.status}>
                        {(s) => (
                          <span class="command-palette-status">
                            <span aria-hidden="true">{s().icon}</span> {s().text}
                          </span>
                        )}
                      </Show>
                      <div class="command-palette-main">
                        {/* Jump rows hold user text in any script; dir="auto"
                            gives each string its own direction. A translated
                            command label keeps the UI direction. */}
                        <span class="command-palette-label" dir={cmd.detail ? "auto" : undefined}>
                          {cmd.label}
                        </span>
                        <Show when={cmd.detail}>
                          <div class="command-palette-detail">
                            <span dir="auto">{cmd.detail}</span>
                            <Show when={cmd.path}>
                              <span dir="ltr">{cmd.path}</span>
                            </Show>
                          </div>
                        </Show>
                      </div>
                      {/* dir="ltr": an accelerator keeps its order in an RTL UI. */}
                      <Show when={cmd.shortcut}>
                        <kbd class="command-palette-hint" dir="ltr">{cmd.shortcut}</kbd>
                      </Show>
                    </div>
                  )}
                </For>
              </div>
            </Show>
          </div>
        </div>
      </div>
    </Show>
  );
}
