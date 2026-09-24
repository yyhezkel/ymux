//go:build linux

package term

// pty_linux.go — the pseudo-terminal, opened straight against /dev/ptmx.
//
// Why not github.com/creack/pty, the obvious choice: the daemon ships as two
// committed binaries (resources/ymux-server-linux-{x64,arm64}) and every
// dependency bump has to travel through go.sum, which cannot be hand-edited
// without a Go toolchain — and this repo builds on CI only (Rule #17).
// golang.org/x/sys is ALREADY in the module graph (gopsutil pulls it), so
// using it costs nothing and adds no new supply-chain surface. The ioctl
// numbers below are asm-generic, identical on amd64 and arm64, which are the
// only two targets that ship.

import (
	"fmt"
	"os"
	"os/exec"
	"syscall"

	"golang.org/x/sys/unix"
)

// PTY is a running child process attached to a pseudo-terminal.
type PTY struct {
	master *os.File
	cmd    *exec.Cmd
}

// StartPTY opens a PTY pair and starts name+args on it as a session leader
// with the slave as its controlling terminal — which is precisely what `tmux
// attach` requires and the reason a plain exec.Cmd with pipes cannot host one.
//
// cols/rows seed the window size so the first repaint tmux sends is already
// the right shape; a zero pair falls back to 80x24 rather than 0x0, which tmux
// renders as an unusable single line.
func StartPTY(name string, args, env []string, cols, rows uint16) (*PTY, error) {
	if cols == 0 || rows == 0 {
		cols, rows = 80, 24
	}

	master, err := os.OpenFile("/dev/ptmx", os.O_RDWR|syscall.O_NOCTTY, 0)
	if err != nil {
		return nil, fmt.Errorf("open ptmx: %w", err)
	}
	fail := func(e error) (*PTY, error) {
		_ = master.Close()
		return nil, e
	}

	// unlockpt: clear the slave's lock before it can be opened.
	if err := unix.IoctlSetPointerInt(int(master.Fd()), unix.TIOCSPTLCK, 0); err != nil {
		return fail(fmt.Errorf("unlockpt: %w", err))
	}
	// ptsname: ask for the slave's number rather than parsing anything.
	n, err := unix.IoctlGetInt(int(master.Fd()), unix.TIOCGPTN)
	if err != nil {
		return fail(fmt.Errorf("ptsname: %w", err))
	}

	slave, err := os.OpenFile(fmt.Sprintf("/dev/pts/%d", n), os.O_RDWR|syscall.O_NOCTTY, 0)
	if err != nil {
		return fail(fmt.Errorf("open pts: %w", err))
	}

	if err := unix.IoctlSetWinsize(int(master.Fd()), unix.TIOCSWINSZ,
		&unix.Winsize{Row: rows, Col: cols}); err != nil {
		_ = slave.Close()
		return fail(fmt.Errorf("winsize: %w", err))
	}

	cmd := exec.Command(name, args...) // #nosec G204 — argv array, Rule #3
	cmd.Env = env
	cmd.Stdin, cmd.Stdout, cmd.Stderr = slave, slave, slave
	// Setsid makes the child a session leader; Setctty then makes fd 0 (the
	// slave, via Stdin) its controlling terminal. Both are required — without
	// Setctty the child has a tty on its fds but no controlling terminal, and
	// tmux refuses to attach with "open terminal failed: not a terminal".
	cmd.SysProcAttr = &syscall.SysProcAttr{Setsid: true, Setctty: true, Ctty: 0}

	if err := cmd.Start(); err != nil {
		_ = slave.Close()
		return fail(fmt.Errorf("start: %w", err))
	}
	// The parent's copy of the slave must go, or the master never sees EOF
	// when the child exits and the read pump would block forever.
	_ = slave.Close()

	return &PTY{master: master, cmd: cmd}, nil
}

// Read delivers output from the child.
func (p *PTY) Read(b []byte) (int, error) { return p.master.Read(b) }

// Write sends input to the child.
func (p *PTY) Write(b []byte) (int, error) { return p.master.Write(b) }

// Resize updates the window size and signals SIGWINCH to the foreground group.
func (p *PTY) Resize(cols, rows uint16) error {
	if cols == 0 || rows == 0 {
		return nil
	}
	return unix.IoctlSetWinsize(int(p.master.Fd()), unix.TIOCSWINSZ,
		&unix.Winsize{Row: rows, Col: cols})
}

// Close kills the child and releases the PTY. For a `tmux attach` child this
// detaches the client; the tmux SESSION and everything running in it survive,
// which is the entire contract of this package.
//
// Closing the master first is what makes the kill reliable: the child's next
// read or write gets EIO and it exits on its own, so the signal is a backstop
// rather than the mechanism.
func (p *PTY) Close() error {
	err := p.master.Close()
	if p.cmd.Process != nil {
		_ = p.cmd.Process.Kill()
		_, _ = p.cmd.Process.Wait()
	}
	return err
}
