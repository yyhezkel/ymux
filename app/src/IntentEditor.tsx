import { createEffect, createSignal, on } from "solid-js";
import { t } from "./i18n";

// BRIEF / Phase 103: the 🎯 intent one-liner editor, shared by the
// Briefing card and the Context Rail. Enter/blur save, plus an explicit
// Save button whose disabled state doubles as "saved ✓" (beta feedback:
// a field that saves invisibly reads as one that doesn't save at all).
// An empty value clears the intent.

interface Props {
  intent: string | null | undefined;
  onSave: (text: string) => void;
  /** Extra class on the wrapping label (layout differs per host). */
  class?: string;
}

export function IntentEditor(p: Props) {
  const saved = () => p.intent ?? "";
  const [draft, setDraft] = createSignal(saved());
  // Follow external changes (another surface saved, workspace switched)
  // without waiting for a remount.
  createEffect(on(saved, (v) => setDraft(v), { defer: true }));

  const saveIfChanged = () => {
    const text = draft().trim();
    if (text !== saved()) p.onSave(text);
  };
  const isSaved = () => draft().trim() === saved();

  return (
    <label class={`briefing-intent ${p.class ?? ""}`}>
      <span class="briefing-intent-label">🎯 {t("briefing.intent.label")}</span>
      <div class="briefing-intent-row">
        <input
          type="text"
          dir="auto"
          maxLength={500}
          placeholder={t("briefing.intent.placeholder")}
          value={draft()}
          onInput={(e) => setDraft(e.currentTarget.value)}
          onBlur={saveIfChanged}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              saveIfChanged();
            }
            e.stopPropagation();
          }}
        />
        <button
          class="primary briefing-intent-save"
          disabled={isSaved()}
          onClick={saveIfChanged}
        >
          {isSaved() ? t("briefing.intent.saved") : t("briefing.intent.save")}
        </button>
      </div>
    </label>
  );
}
