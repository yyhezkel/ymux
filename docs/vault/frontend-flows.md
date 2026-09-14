---
vault: frontend-flows
covers:
  - app/src/SetupWizard.tsx
  - app/src/LocalSetupFlow.tsx
  - app/src/QuickLocalCreateFlow.tsx
  - app/src/ProvisionNewServerFlow.tsx
  - app/src/ConnectExistingFlow.tsx
  - app/src/QuickSshCreateFlow.tsx
  - app/src/CreateWorkspaceModal.tsx
  - app/src/SshConnectionFields.tsx
  - app/src/SshKeyOfferModal.tsx
  - app/src/WorkspaceExtrasFields.tsx
  - app/src/ProjectFolderModal.tsx
  - app/src/ConfirmDeleteWorkspace.tsx
  - app/src/NotesModal.tsx
  - app/src/DirPicker.tsx
  - app/src/SettingsModal.tsx
  - app/src/VersionManager.tsx
  - app/src/provisioningTypes.ts
---

# Wizards, modals, settings

Everything with an OK button. ~5,900 lines, and the shape is set by **Phase 80's unified
setup wizard**: one "+ new workspace" button opens `SetupWizard`, which is a mode tree,
and each leaf is its own flow component.

## The mode tree

```
SetupWizard  (318)
├── local  → new       → LocalSetupFlow        (651)  detect + install, then create
│         → existing   → QuickLocalCreateFlow  (247)  plain cmd/PowerShell/Git-Bash shell
└── server → new       → ProvisionNewServerFlow(628)  provision a fresh box
          → existing   → ConnectExistingFlow   (406)  multi-machine SSH onboarding
                      → QuickSshCreateFlow     (106)  "I already have a key"
```

`SetupWizard` owns the mode picker. That is a **move**, not a duplication:
`ProvisionNewServerFlow` is the former ProvisioningWizard with the picker stripped out,
and it no longer hosts the "existing" flows at all.

- **`LocalSetupFlow`** — detects git / node / Claude Code / codex / gemini plus the
  platform's multiplexer, offers to install what is missing (winget / Homebrew /
  official installers / `npm -g`), installs ymux hooks for local Claude Code, and
  finishes by creating a local workspace to land in. Progress arrives as
  `local-setup:progress` events from `local_setup.rs`.
- **`ProvisionNewServerFlow`** — same step-card UI, fed by `provisioning:progress` from
  `provisioning.rs`. Per-step retry/skip; a failed step does not abort the run.
- **`ConnectExistingFlow`** — auth → discover → choose. The Rust side
  (`connect_existing_discover` / `connect_existing_execute`) does the work; this is the
  chooser.
- **`QuickSshCreateFlow`** — records connection details (ssh-config import, key picker,
  permissions fix, test connection) and creates the workspace. It **does not touch the
  remote host.**

`provisioningTypes.ts` (38) mirrors the Rust step/progress shapes.

## `CreateWorkspaceModal.tsx` (451) — **edit-only**

Creation moved to `SetupWizard`. This modal is what the sidebar's edit action and the
palette rename open. The SSH form and the extras block are **shared** with the wizard
rather than duplicated:

- **`SshConnectionFields.tsx` (492)** — the SSH form. **The parent owns the form state**
  (via `createSshFormState`) so it can hydrate from an existing workspace or read the
  built `Connection` on submit; everything transient stays inside the component.
- **`WorkspaceExtrasFields.tsx` (91)** — the setup/teardown/env block, same ownership
  split.

## Dialogs

- **`SshKeyOfferModal.tsx` (172)** — when the user authenticates by password, the backend
  emits `ssh-key-offer`; this asks whether to generate an ed25519 pair and install the
  public half into the remote's `~/.ssh/authorized_keys`.
- **`ProjectFolderModal.tsx` (266)** — two dialogs over one chrome: `pin` (type a folder
  path — usually a repo, but a directory without git pins too, demoted, via
  `project_folder_probe`'s verdict) and `worktree` (create a worktree inside a pinned
  folder).
- **`ConfirmDeleteWorkspace.tsx` (123)** — deleting a workspace takes its **whole
  subtree**: pinned project folders, the worktree workspaces under them, and any live
  remote sessions. `window.confirm` was carrying that danger in an unstyled grey OS box
  nobody reads. This spells out what is about to go.
- **`DirPicker.tsx` (176)** — remote directory browser over the workspace's live SSH
  session. Its markup, CSS classes (`dir-picker-*`), i18n keys (`connect.dirPicker.*`)
  and localStorage recents were lifted out of `PaneView`, where the dialog had been
  sitting unreachable.
- **`NotesModal.tsx` (273)** — notes CRUD against `notes.rs`.

## `SettingsModal.tsx` (1,869)

The whole settings surface in tabs — theme, fonts, terminal, RTL profiles, hooks,
notifications, logs, Claude, updates, shortcuts. Reads and writes through
`settings.ts` (the typed mirror; `src-tauri/src/settings.rs` owns the canonical schema)
and reacts to `settings:changed`, so a `ymux settings set` from the CLI updates the open
modal. The General tab carries BRIEF's "Briefing card" section (two opt-in trigger
toggles + two minute thresholds); its writes always spread the COMPLETE `brief`
group over `DEFAULT_BRIEF_SETTINGS` — the `setRtlField` lesson applied to a new
group.

The RTL block is the one worth knowing: a `local` / `remote` profile pill, then the
`rtl_mode` radios — `auto_per_line`, `force_rtl`, `bidi_reorder`, `off` — over the
`auto_direction` / `mirror_arrows_rtl` / `tui_owns_bidi` / `direction_policy` knobs.
`setRtlField` writes a **complete** profile object rather than a partial: Rust's
per-field `serde(default)` would otherwise resurrect type-defaults instead of
profile-defaults. `RTL_FIELD_DEFAULTS` holds `auto_per_line` for both profiles and
did not move when `force_rtl` was added (2026-08-23) — the new mode is opt-in.

Two tabs are deliberately **separate components** so they do not bloat this file:
`AddonsTab` and `YmuxToolsTab` (see `frontend-panes.md`).

The fonts section has two notices that are mirror images, and the second exists because
of a gap in the first. `FontMissingNotice` renders only while the picked family is
MISSING, and offers Install — so an INSTALLED font had nowhere to hang a control, which
is why removing one used to mean hand-deleting from `%LOCALAPPDATA%` and HKCU.
`FontInstalledList` is the other half: a compact "Installed by ymux" list under both
pickers, one Remove button per row, rendering nothing when nothing is installed. It has
to be a list rather than a button beside the picker, because the picker only ever shows
one family. Guarded by `window.confirm` — it deletes files and the way back is a
multi-MB download. A partial removal is a normal outcome, not an error: Windows will not
delete a font a running app has open, so the `failed` list gets its own message.
**Both handlers re-read `fontCatalog()` as well as `listSystemFonts()`**, since the
installed flag lives on the catalog and the row would otherwise not appear or disappear
until Settings was reopened.

**The Shortcuts tab is 28 recordable rows in five labelled groups**, driven by
`SHORTCUT_GROUPS` from `shortcuts.ts` — the group list is the UI's row order, and a
unit test asserts it covers every action id exactly once, so a binding cannot exist in
the schema with no row. `ShortcutRow` is the click-to-record picker: focus it, press
the combination, `formatEvent` stores the canonical accelerator, Esc cancels. It
**calls `stopPropagation`**, and that is not optional — `App.tsx` listens for keydown
on `window` in the bubble phase, so without it the combination being *recorded* also
fires the action it is bound to, which since Phase 87 means recording `Ctrl+Shift+W`
closes the active pane. `preventDefault` alone does not stop propagation. Rows whose
accelerator is claimed by another action are outlined in `--w-error`
(`conflictingAccels`), and a read-only list at the bottom shows the bindings that
cannot be rebound at all (`Ctrl+1..9`, `Escape`, the editor's and browser pane's own
keys) so they stop being invisible.

**Push-to-talk is edited in the AI tab, not the Shortcuts tab**, because it is stored
at `settings.stt.push_to_talk_hotkey` rather than in `settings.shortcuts`. It uses the
same `ShortcutRow` (it was a free-text box until Phase 87 — the accelerator had to be
typed, and a typo produced a hotkey that silently never fired), and it is passed into
`conflictingAccels` explicitly so a clash across the two schemas is still reported.

**`VersionManager.tsx` (228)** is the Updates tab's list: every published release,
install any of them (including a downgrade, with a warning), and pick a release channel.
Backed by `updater_list_versions` / `updater_install_version`.

## Invariants

- **Rule #5** — no `any`; `invoke` return types explicit.
- **The parent owns form state.** Every shared field block follows the
  `SshConnectionFields` pattern, so hydration and submit both live in one place.
- A wizard step that shells out reaches the backend through a Tauri command — the
  frontend never builds a command line.
- **Rule #2** — a password or passphrase typed in a flow is passed to the backend and
  not retained in a signal after submit.
- Every new field added to the SSH form must also be handled by the edit modal, since
  both render the same component. That is the reason it is a component.

## Read the source when

You need the exact settings tab layout, an install step's label copy, or the i18n keys.
The engines behind these flows are in `backend-wizards.md` and `backend-remote.md`; the
user-facing config surface is documented in `docs/CONFIG.md`.
