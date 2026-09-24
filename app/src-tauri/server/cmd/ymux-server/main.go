// Command ymux-server is the ymux server daemon (Phase 77) — the former
// ymux-insights, restructured into internal/* subsystems behind core
// interfaces. S1.b wires the metrics side (insights + config + auth + api);
// chat/pairing/hooks join in S1.c.
package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"syscall"
	"time"

	"ymux-server/internal/api"
	"ymux-server/internal/chat"
	"ymux-server/internal/config"
	"ymux-server/internal/core"
	"ymux-server/internal/desktop"
	"ymux-server/internal/push"
	"ymux-server/internal/files"
	"ymux-server/internal/hooks"
	"ymux-server/internal/insights"
	"ymux-server/internal/logging"
	"ymux-server/internal/logs"
	"ymux-server/internal/term"
	"ymux-server/internal/workspace"
)

func main() {
	// Phase 79.D: unified slog logging. Created before Setup — the shared
	// writer indirection re-points it at the log file once Setup runs.
	logger := logging.New("SRV:MAIN")

	if len(os.Args) > 1 {
		switch os.Args[1] {
		case "--version", "-v", "version":
			// Same output shape as legacy `ymux-insights --version` so the
			// desktop's version probe keeps working across the rename.
			fmt.Printf("ymux-server %s\n", core.Version)
			return
		case "openapi":
			// Emit the generated OpenAPI spec to stdout and exit. The SDK
			// pipeline (sdk-gen/) and its CI drift-guard use this — no running
			// server or data dir needed (nil providers; handlers never run).
			// All subsystems present (nil-backed) so every SDK-facing op is in the
			// spec — registration only reflects types; no store is touched.
			srv := api.NewServer("", 0, api.Deps{
				Files:     files.NewService(nil),
				Logs:      logs.NewService(nil),
				Chat:      chat.NewChatAPI(nil, nil, ""),
				Workspace: workspace.NewService(workspace.NewManager(nil, nil), ""),
			})
			b, err := srv.OpenAPISpec()
			if err != nil {
				logger.Error("openapi spec generation failed", "err", err)
				os.Exit(1)
			}
			_, _ = os.Stdout.Write(b)
			return
		}
	}

	home, homeErr := os.UserHomeDir()
	// Phase 77 S5: the data dir is ~/.ymux/server. A 1.x install kept it at
	// ~/.ymux/insights; the daemon migrates it in place on first boot (below),
	// preserving token + chat.db (paired devices) + workspace/metrics DBs + logs.
	defBase := filepath.Join(home, ".ymux", "server")
	// Tried in order, first one holding real data wins. The `.winmux` pair is
	// the winmux → ymux rename: a daemon provisioned before it kept its token,
	// chat.db (paired devices) and metrics under the old home, and starting
	// clean there would silently unpair every phone.
	legacyBases := []string{
		filepath.Join(home, ".ymux", "insights"),
		filepath.Join(home, ".winmux", "server"),
		filepath.Join(home, ".winmux", "insights"),
	}

	fs := flag.NewFlagSet("serve", flag.ExitOnError)
	port := fs.Int("port", 7879, "localhost TCP port for the API")
	base := fs.String("dir", defBase, "data directory (db, token, log)")
	interval := fs.Int("interval", 5, "sample interval, seconds")
	filesRoot := fs.String("files-root", home, "sandbox root for the Files API (default $HOME)")
	args := os.Args[1:]
	if len(args) > 0 && args[0] == "serve" {
		args = args[1:]
	}
	_ = fs.Parse(args)

	// Migrate only when using the default dir (an explicit --dir opts out).
	if *base == defBase {
		for _, legacyBase := range legacyBases {
			migrated, err := config.MigrateDataDir(legacyBase, defBase)
			if err != nil {
				logger.Warn("data-dir migration failed (continuing)", "from", legacyBase, "to", defBase, "err", err)
				continue
			}
			if migrated {
				logger.Info("migrated data dir", "from", legacyBase, "to", defBase)
				break
			}
		}
	}

	if err := os.MkdirAll(*base, 0o755); err != nil {
		logger.Error("mkdir data dir failed", "dir", *base, "err", err)
		os.Exit(1)
	}

	logPath := filepath.Join(*base, "insights.log")
	config.RotateIfBig(logPath, 1<<20)
	lf, err := os.OpenFile(logPath, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o644)
	if err == nil {
		defer lf.Close()
		logging.Setup(lf)
	} else {
		logging.Setup(os.Stderr)
	}
	logger.Info("ymux-server starting", "version", core.Version, "port", *port, "dir", *base, "interval_s", *interval)

	// systemd gives the daemon a minimal PATH; merge the user's login-shell PATH
	// so subprocesses (the claude_chat engine) resolve `claude` like the terminal.
	config.AugmentUserPath()
	// Diagnostic: surface whether `claude` is resolvable after the augment, and
	// the PATH we ended up with — so a "claude not found" mobile error is
	// debuggable from Monitor → Logs instead of invisible.
	if p, lerr := exec.LookPath("claude"); lerr == nil {
		logger.Info("claude resolved", "path", p)
	} else {
		logger.Warn("claude NOT resolved after PATH augment", "err", lerr, "path_env", os.Getenv("PATH"))
	}

	token := config.LoadOrCreateToken(filepath.Join(*base, "token"))

	store, err := insights.OpenStore(filepath.Join(*base, "metrics.db"))
	if err != nil {
		logger.Error("open metrics store failed", "err", err)
		os.Exit(1)
	}
	defer store.Close()
	store.Sweep() // drop anything older than the retention window on boot

	sm := insights.NewSampler()
	stop := make(chan struct{})
	go insights.RunSampler(store, sm, time.Duration(*interval)*time.Second, stop)
	go config.LogJanitor(logPath, home, stop)
	go insights.PortWatchReaper(stop)
	// Runtime log level: ~/.ymux/log-level (debug/info/warn/error), polled
	// every 30s. Missing/unknown → Info. Skipped when home is unresolvable.
	if homeErr == nil {
		go logging.WatchLevelFile(filepath.Join(home, ".ymux", "log-level"), stop)
	}

	svc := insights.NewService(store, sm, logPath)

	// Phase 69 — mobile Claude chat subsystem (separate chat.db). On any setup
	// error we log and continue serving metrics; chat just stays off.
	var chatAPI *chat.ChatAPI
	var chatMgr *chat.SessionManager // hoisted so the workspace bridge can reach it
	chatStore, cerr := chat.OpenChatStore(filepath.Join(*base, "chat.db"))
	if cerr != nil {
		logger.Warn("chat store open failed, chat disabled", "err", cerr)
	} else {
		defer chatStore.Close()
		chatMgr = chat.NewSessionManager(chatStore)
		chatAPI = chat.NewChatAPI(chatMgr, chatStore, token)
		// Phase 96: browser-initiated pairing needs a human's Allow/Deny, and
		// the only human here is at the ymux desktop. Wire the outbound tunnel
		// client as a closure so `chat` never imports `desktop` — same shape as
		// SetPushLister / SetDeviceAuth. Unresolvable home ⇒ left nil, and a
		// pairing request then fails closed with "ymux is not connected".
		if homeErr == nil {
			chatAPI.SetApprovalAsker(func(reqID, title, summary string, payload map[string]any, wait int) (string, error) {
				d, err := desktop.AskApproval(home, reqID, title, summary, payload, wait)
				return string(d), err
			})
		} else {
			logger.Warn("browser pairing disabled: home directory unresolvable")
		}
		hooks.Start(chatMgr) // thin listener → SessionManager.HandleHookConn (cycle-safe)
		go chat.RunSessionSweeper(chatMgr, stop)
		logger.Info("Claude chat subsystem enabled")
	}

	// Files API (S2) — sandboxed to --files-root ($HOME by default).
	var filesSvc *files.Service
	if fp, ferr := files.NewLocalFiles(*filesRoot, 0); ferr != nil {
		logger.Warn("files init failed, Files API disabled", "err", ferr)
	} else {
		filesSvc = files.NewService(fp)
		logger.Info("files API enabled", "root", fp.Root())
	}

	// Logs API (S2) — per-client log tree under <dir>/logs, plus the "server"
	// pseudo-client that surfaces this daemon's own log.
	var logsSvc *logs.Service
	if lstore, lerr := logs.NewStore(*base, logPath); lerr != nil {
		logger.Warn("logs init failed, Logs API disabled", "err", lerr)
	} else {
		logsSvc = logs.NewService(lstore)
		go lstore.RunJanitor(stop)
		logger.Info("logs API enabled")
	}

	// Workspace API (S3) — shared-state model (sessions + subscribers + pending)
	// in its own workspace.db.
	var wsSvc *workspace.Service
	var pushSrv *push.Server
	if wstore, werr := workspace.OpenStore(filepath.Join(*base, "workspace.db")); werr != nil {
		logger.Warn("workspace store open failed, Workspace API disabled", "err", werr)
	} else {
		defer wstore.Close()
		// Native push (§4): self-hosted delivery over each device's long-lived
		// WS, queued per-device when offline. Implements NotificationSender; the
		// workspace manager fans out to it. No external push provider.
		var notifier core.NotificationSender = core.NoopSender{}
		if chatAPI != nil {
			pushSrv = push.New(push.Deps{
				ResolveToken: chatAPI.ResolveToken,
				Enqueue:      chatAPI.EnqueuePush,
				PendingAfter: func(d string, c int64) []push.QueuedEvent {
					pend := chatAPI.PendingPush(d, c)
					out := make([]push.QueuedEvent, 0, len(pend))
					for _, pe := range pend {
						out = append(out, push.QueuedEvent{PushSeq: pe.PushSeq, Ts: pe.Ts, Event: pe.Event})
					}
					return out
				},
				Ack: chatAPI.AckPush,
			})
			notifier = pushSrv
			// Prune the pending queue (24h TTL) at boot + hourly.
			go func() {
				const ttl = int64(24 * 3600)
				prune := func() { chatAPI.PrunePush(time.Now().Unix() - ttl) }
				prune()
				t := time.NewTicker(time.Hour)
				defer t.Stop()
				for {
					select {
					case <-stop:
						return
					case <-t.C:
						prune()
					}
				}
			}()
			logger.Info("native push enabled")
		}
		wmgr := workspace.NewManager(wstore, notifier)
		// Out-of-band push fan-out targets come from the chat device store.
		if chatAPI != nil {
			wmgr.SetPushLister(chatAPI)
		}
		// Backward-compat: a stable "default" workspace so legacy chat sessions
		// (still served at /api/claude/*) have a home in the workspace model as
		// the deeper chat↔workspace merge lands in a later sprint.
		if _, e := wmgr.EnsureWorkspace(workspace.DefaultID, "default"); e != nil {
			logger.Warn("ensure default workspace failed", "err", e)
		}
		wsSvc = workspace.NewService(wmgr, token)
		// Accept paired-device tokens on the subscribe WS (matches the REST
		// surface) so a phone's long-term token works on the stream, not just
		// the shared desktop token.
		if chatAPI != nil {
			wsSvc.SetDeviceAuth(chatAPI.TokenValid)
		}
		// §16 engine↔substrate bridge: run Claude for `claude_chat` sessions —
		// client user_input spawns/feeds a Claude process whose output streams
		// back as workspace frames.
		if chatMgr != nil {
			wmgr.SetDriver(chat.NewWorkspaceBridge(chatMgr, wmgr))
			logger.Info("claude_chat engine bridge enabled")
			// beta.3 Fix 3: chat sessions surface a workspace-name label on
			// emitted hook_request events. Wired here (both managers exist)
			// so tests can build a chat.SessionManager without a workspace.
			chatMgr.SetWorkspaceResolver(wmgr.WorkspaceNameByID)
		}
		logger.Info("workspace API enabled")
	}

	// Terminal API (Phase 95) — tmux session management plus the attach WS that
	// gives a browser a real terminal. No store and no state: tmux is the
	// truth, a session's identity is its tmux name, and each attach is one
	// `tmux attach` process (internal/term).
	//
	// Access needs auth.ScopeShellAttach, which "all" deliberately does NOT
	// imply — so every phone paired before this release keeps working and none
	// of them gains a shell. The owner/shared token always passes.
	//
	// An unresolvable home only costs the session-meta labels (LoadMeta
	// degrades to an empty map); tmux itself still lists.
	termSvc := term.NewService(token, home)
	if chatAPI != nil {
		termSvc.SetScopeResolver(chatAPI.DeviceScopes)
	} else {
		logger.Warn("terminal API: chat disabled, only the shared token is accepted")
	}
	logger.Info("terminal API enabled", "device_scopes", chatAPI != nil)

	srv := api.NewServer(token, *port, api.Deps{
		Insights: svc, Chat: chatAPI, Files: filesSvc, Logs: logsSvc,
		Workspace: wsSvc, Push: pushSrv, Term: termSvc,
	})

	go func() {
		if err := srv.Run(); err != nil {
			logger.Error("http server failed", "err", err)
			os.Exit(1)
		}
	}()
	logger.Info("API listening", "addr", fmt.Sprintf("127.0.0.1:%d", *port))

	sig := make(chan os.Signal, 1)
	signal.Notify(sig, syscall.SIGINT, syscall.SIGTERM)
	<-sig
	logger.Info("ymux-server stopping (draining connections)")
	close(stop) // stop samplers/janitors/sweepers

	// Graceful HTTP drain so an in-flight metrics/file request isn't cut off on
	// restart; hard-stop after the deadline.
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := srv.Shutdown(ctx); err != nil {
		logger.Warn("shutdown drain failed", "err", err)
	}
	logger.Info("ymux-server stopped")
}
