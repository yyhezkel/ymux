// Right-click Copy text: xterm selection, else the last OSC 52 write when the
// app (zellij) owns the mouse and drag-select never reaches xterm.
//
// Pure, so it is unit-tested (termMenuCopy.test.ts); terminalInstance.ts keeps
// the last OSC 52 text. `lastOsc52` is RAW -- copyToClipboard does the
// visual->logical flip once.
export function menuCopyText(
  xtermSelection: string,
  appOwnsMouse: boolean,
  lastOsc52: string,
): string {
  if (xtermSelection) return xtermSelection;
  return appOwnsMouse ? lastOsc52 : "";
}
