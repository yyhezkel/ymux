// select_all (Ctrl+Shift+A) binding -- pure, zero-import so node can unit-test it.

export interface SelectAllTerm {
  selectAll(): void;
}

export interface SelectAllDeps {
  activePaneId: () => string | null;
  termFor: (paneId: string) => SelectAllTerm | undefined;
}

// True when the key event came from inside an xterm container.
export const inTerminal = (e: { target: EventTarget | null }): boolean =>
  !!(e.target as HTMLElement | null)?.closest?.(".terminal-container");

export function makeSelectAllBinding(deps: SelectAllDeps): {
  when: (e: { target: EventTarget | null }) => boolean;
  run: (e: { preventDefault(): void }) => void;
} {
  return {
    when: (e) => inTerminal(e) && !!deps.activePaneId(),
    run: (e) => {
      e.preventDefault();
      const pid = deps.activePaneId();
      if (pid) deps.termFor(pid)?.selectAll();
    },
  };
}
