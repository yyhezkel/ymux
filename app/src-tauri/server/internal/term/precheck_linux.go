//go:build linux

package term

import (
	"os/exec"
	"syscall"
	"time"
)

// precheckProcGroup puts the precheck in its own process group. On timeout
// the whole group gets SIGKILL, so a child the precheck started (a shell's
// `sleep`, a `git` helper) cannot outlive it.
func precheckProcGroup(cmd *exec.Cmd) {
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	cmd.Cancel = func() error {
		// Setpgid makes the group id equal to the leader's pid.
		return syscall.Kill(-cmd.Process.Pid, syscall.SIGKILL)
	}
	// Output goes to /dev/null, so no pipe can hold Wait. This is a guard.
	cmd.WaitDelay = 5 * time.Second
}
