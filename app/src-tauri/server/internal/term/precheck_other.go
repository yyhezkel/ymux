//go:build !linux

package term

import "os/exec"

// precheckProcGroup does nothing off Linux. The daemon ships on Linux only;
// this file keeps the package compiling elsewhere. On timeout,
// exec.CommandContext kills the direct child only.
func precheckProcGroup(cmd *exec.Cmd) {}
