package beachcomber_test

import (
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"testing"
	"time"

	beachcomber "github.com/NavistAu/beachcomber/sdks/go"
)

// This SDK now binds to libbeachcomber's C ABI (see ffi.go), so its unit
// tests can no longer mock the wire protocol directly — there is no wire
// protocol code left in this package to mock. Instead these tests spawn a
// real `comb daemon` and drive it through the real library, the same shape
// cmd/conformance uses. TestMain below arranges for `go test ./...` (no env
// vars) to find that library and daemon at their repo-relative debug build
// locations, mirroring cmd/conformance/main.go's own COMB_BIN fallback.

func sdkDir() string {
	_, thisFile, _, _ := runtime.Caller(0)
	return filepath.Dir(thisFile)
}

func repoDebugDir() string {
	return filepath.Join(sdkDir(), "..", "..", "target", "debug")
}

func defaultCombBin() string {
	return filepath.Join(repoDebugDir(), "comb")
}

func defaultLibPath() string {
	name := "libbeachcomber.so"
	if runtime.GOOS == "darwin" {
		name = "libbeachcomber.dylib"
	}
	return filepath.Join(repoDebugDir(), name)
}

func fileExists(p string) bool {
	_, err := os.Stat(p)
	return err == nil
}

func TestMain(m *testing.M) {
	if os.Getenv("BEACHCOMBER_LIB") == "" && fileExists(defaultLibPath()) {
		os.Setenv("BEACHCOMBER_LIB", defaultLibPath())
	}
	os.Exit(m.Run())
}

func combBin() string {
	if b := os.Getenv("COMB_BIN"); b != "" {
		return b
	}
	return defaultCombBin()
}

// requireBuiltArtifacts skips the test when comb/libbeachcomber haven't been
// built, rather than failing — matches this repo's convention of building
// before running the SDK conformance runner.
func requireBuiltArtifacts(t *testing.T) {
	t.Helper()
	if !fileExists(combBin()) {
		t.Skipf("comb binary not found at %s; run `cargo build` first", combBin())
	}
	if os.Getenv("BEACHCOMBER_LIB") == "" {
		t.Skip("BEACHCOMBER_LIB not set and no default build found; run `cargo build -p libbeachcomber-ffi` first")
	}
}

// startDaemon starts a fresh comb daemon on a temp socket and returns the
// socket path. The daemon is killed on test cleanup.
func startDaemon(t *testing.T) string {
	t.Helper()
	requireBuiltArtifacts(t)

	dir := t.TempDir()
	sock := filepath.Join(dir, "sock")
	cmd := exec.Command(combBin(), "daemon", "--socket", sock)
	cmd.Stderr = os.Stderr
	if err := cmd.Start(); err != nil {
		t.Fatalf("start daemon: %v", err)
	}
	t.Cleanup(func() {
		if cmd.Process != nil {
			cmd.Process.Kill()
			cmd.Wait()
		}
	})

	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		if conn, err := net.DialTimeout("unix", sock, 100*time.Millisecond); err == nil {
			conn.Close()
			return sock
		}
		time.Sleep(20 * time.Millisecond)
	}
	t.Fatalf("daemon did not create socket within 5s at %s", sock)
	return ""
}

// requireClient starts a fresh daemon and returns a Client wired to it.
func requireClient(t *testing.T) *beachcomber.Client {
	t.Helper()
	return beachcomber.NewClientWithPath(startDaemon(t))
}
