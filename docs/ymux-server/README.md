# ymux-server

The ymux server daemon (2.0.0) — formerly `ymux-insights`, restructured in
Phase 77 into a module with clean `internal/*` subsystems behind `core`
interfaces. Runs on the remote host the desktop connects to; exposes a
localhost HTTP + WebSocket API behind the ymux tunnel, bearer-token gated.

## Subsystems

| Package | Surface | Notes |
|---|---|---|
| `internal/insights` | metrics / docker / processes / hygiene | desktop Monitor (Rust client); not in the SDK spec |
| `internal/files` | `/api/v2/files/*` | sandboxed filesystem (traversal + symlink-escape rejected) |
| `internal/logs` | `/api/v2/logs/*` | per-client log tree + `server` pseudo-client + SSE tail |
| `internal/workspace` | `/api/v2/workspace/*` + WS `subscribe` | shared-state sessions (8a attach, 8b hook broadcast) |
| `internal/chat` | pairing (`/api/pairing/*`) | Claude session engine kept internal; legacy chat HTTP → 410 |
| `internal/api` | front door | auth, version/health, generated OpenAPI + WS frame specs |

Contract + SDKs: [API.md](API.md), [CLIENTS.md](CLIENTS.md), [../../sdk-gen](../../sdk-gen).

## Run

```
ymux-server [serve] [--port 7879] [--dir ~/.ymux/insights] \
              [--interval 5] [--files-root $HOME]
ymux-server --version     # prints "ymux-server 2.0.0"
ymux-server openapi       # prints the generated OpenAPI spec (for sdk-gen)
```

Data dir (`--dir`, default `~/.ymux/insights`) holds `token`, `metrics.db`,
`chat.db`, `workspace.db`, `logs/`, and the rotating `insights.log`. The bearer
token is generated on first boot (`<dir>/token`).

## Build

CGO-free (`modernc.org/sqlite`), so it cross-compiles cleanly:

```
cd app/src-tauri/server
GOOS=linux GOARCH=amd64 CGO_ENABLED=0 go build -trimpath -buildvcs=false -ldflags="-s -w" \
  -o ../resources/ymux-server-linux-x64 ./cmd/ymux-server
GOOS=linux GOARCH=arm64 CGO_ENABLED=0 go build -trimpath -buildvcs=false -ldflags="-s -w" \
  -o ../resources/ymux-server-linux-arm64 ./cmd/ymux-server
```

`-trimpath` keeps the build-host username out of the binary. `-buildvcs=false`
keeps the commit stamp out, so the same source always gives the same bytes.
Use exactly the Go version on the `toolchain` line of `go.mod`: any other
version gives different bytes and fails the CI gate. The desktop embeds
these two binaries (`include_bytes!` in `src/addons.rs`) and SFTP-uploads the
right arch to `~/.ymux/bin/ymux-server` on install.

**You usually should not run those two commands.** Per CLAUDE.md Rule #17
builds happen on CI: `ci-windows.yml` runs the identical cross-build on every
push/PR and uploads the result as the `ymux-server-linux` artifact. Rebaking =
download that artifact into `../resources/` and commit it alongside the Go
change. `scripts/rebake-server.sh [run-id]` does that for the newest
ci-windows run of HEAD, and refuses a run built from another commit. The same workflow byte-compares the committed blobs with its own
build of the same commit and *fails* on any difference, because the committed
blobs are what ships — the Go source is never compiled by the desktop build.
To bump Go: change the `toolchain` line and rebake in the same commit.

See [DEPLOYMENT.md](DEPLOYMENT.md) for the systemd unit + tunnel, and
[UPGRADE.md](UPGRADE.md) for the 1.x → 2.0 path.
