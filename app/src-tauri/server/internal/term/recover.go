package term

// recover.go — hook routing survives a daemon restart (Phase 111).
//
// The registry (hookreg.go) lives in memory, so a restart — every add-on
// update — used to forget every browser session: they kept running, but
// their hooks were refused as unknown and their lights, feed and gates went
// dead. Two things bring them back:
//
//   - hooks.Start reclaims the previous listener port (<data dir>/hook-port),
//     because YMUX_SOCKET_ADDR is fixed in the environment of every process
//     already running in a session;
//   - RecoverHooks reads each session's identity back from its own tmux
//     environment: the token and pane id set at create, plus YMUX_POLICY and
//     YMUX_WORKSPACE_ID (written since this phase; older sessions recover
//     with policy none and no workspace).
//
// No new store and no secret on disk: tmux already holds the token for the
// hook processes. A session is taken only when its YMUX_SOCKET_ADDR is this
// daemon's listener — desktop sessions carry the same variable names pointed
// at the desktop's reverse tunnel, and must never be claimed.

// RecoverHooks re-registers this daemon's sessions after a restart. Call it
// after the hook listener is up (SetHookAddr) and after SetDataDir.
func (s *Service) RecoverHooks() {
	if s.hooks == nil {
		return
	}
	addr := s.hooks.hookAddr()
	if addr == "" || !s.tmux.SupportsSessionEnv() {
		return
	}
	sessions, err := s.tmux.List()
	if err != nil {
		logger.Warn("hook recovery: tmux list failed", "err", err)
		return
	}
	n := 0
	for _, ss := range sessions {
		if s.hooks.hasSession(ss.Name) {
			continue
		}
		env, err := s.tmux.Environment(ss.Name)
		if err != nil || env["YMUX_SOCKET_ADDR"] != addr {
			continue
		}
		tok, pane := env["YMUX_TUNNEL_TOKEN"], env["YMUX_PANE_ID"]
		if len(tok) != 64 || !ValidPaneID(pane) || s.hooks.paneInUse(pane) {
			continue
		}
		policy := env["YMUX_POLICY"]
		if !validPolicy(policy) {
			policy = policyNone
		}
		ws := env["YMUX_WORKSPACE_ID"]
		if ws != "" && (s.hooks.webws == nil || !s.hooks.webws.exists(ws)) {
			ws = ""
		}
		s.hooks.add(&hookEntry{name: ss.Name, token: tok, paneID: pane, policy: policy, workspaceID: ws})
		n++
	}
	// Counts only: names can carry project names, and the env holds tokens.
	logger.Info("hook routing recovered", "sessions", n, "listed", len(sessions))
}
