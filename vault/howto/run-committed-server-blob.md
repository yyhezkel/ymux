# Run the committed server blob on a side port
- blob: `app/src-tauri/resources/ymux-server-linux-x64`. Committed without the exec bit → `install -m755 <blob> $D/ymux-server`
- flags: `serve --port <p> --dir <d> --files-root <r>` (`app/src-tauri/server/cmd/ymux-server/main.go` flag set: `port` default 7879, `dir`, `interval`, `files-root`)
- always pass an explicit `--dir <mktemp>`: the default dir runs the legacy data-dir migration (main.go, `if *base == defBase`)
- token self-created at `<dir>/token` → `Authorization: Bearer $(cat <dir>/token)`
- routes: `app/src-tauri/server/internal/insights/service.go` route table (`/claude-usage`, `/analytics`, ...)
- why: the deployed `~/.ymux/bin/ymux-server` can lag the tree (2026-10-06: 404 on `/claude-usage` + `/analytics`). The blob is what ships
- cleanup: `pkill -f $D/ymux-server; rm -rf $D`
