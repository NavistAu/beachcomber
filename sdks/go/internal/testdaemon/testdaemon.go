// Package testdaemon starts a comb daemon for this SDK's own tests and its
// conformance runner: one start, one readiness wait.
package testdaemon

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"time"
)

// Start runs `comb daemon` on socketPath and returns once it serves. The
// daemon's managed defaults and log go under socketPath's directory, never the
// caller's real config or state. It says it serves on its `--ready-fd` pipe
// (the child's fd 3, its first ExtraFiles): a newline once its socket is
// served, the pipe closed unwritten if it exits first.
func Start(combBin, socketPath string, timeout time.Duration) (*exec.Cmd, error) {
	ready, signal, err := os.Pipe()
	if err != nil {
		return nil, fmt.Errorf("ready pipe: %w", err)
	}
	defer ready.Close()
	cmd := exec.Command(combBin, "daemon", "--socket", socketPath, "--ready-fd", "3")
	cmd.ExtraFiles = []*os.File{signal}
	cmd.Stdout = os.Stderr // daemon logs to stderr for visibility
	cmd.Stderr = os.Stderr
	// The daemon writes its managed defaults under $XDG_CONFIG_HOME on start
	// (falling back to $HOME/.config) and its log under $XDG_STATE_HOME: all
	// three point into the socket's directory.
	dir := filepath.Dir(socketPath)
	cmd.Env = append(os.Environ(), "XDG_CONFIG_HOME="+dir, "HOME="+dir, "XDG_STATE_HOME="+filepath.Join(dir, ".local", "state"))
	err = cmd.Start()
	signal.Close()
	if err != nil {
		return nil, fmt.Errorf("start daemon: %w", err)
	}

	said := make([]byte, 1)
	ready.SetReadDeadline(time.Now().Add(timeout))
	if n, _ := ready.Read(said); n == 1 && said[0] == '\n' {
		return cmd, nil
	}
	cmd.Process.Kill()
	cmd.Wait()
	return nil, fmt.Errorf("daemon did not serve within %s on %s", timeout, socketPath)
}

// Stop kills the daemon and reaps it.
func Stop(cmd *exec.Cmd) {
	if cmd == nil || cmd.Process == nil {
		return
	}
	cmd.Process.Kill()
	cmd.Wait()
}
