package beachcomber_test

import (
	"os"
	"path/filepath"
	"runtime"
	"testing"
	"time"

	beachcomber "github.com/NavistAu/beachcomber/sdks/go"
	"github.com/NavistAu/beachcomber/sdks/go/internal/testdaemon"
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

// testIOTimeoutMs is how long one read or write of a test's client may take.
// The library's default (100ms) is a latency choice; these tests are about
// correctness, and a loaded machine must not turn a slow answer into a failure.
const testIOTimeoutMs = 30000

// startDaemon starts a fresh comb daemon on a temp socket and returns the
// socket path. The daemon is killed on test cleanup.
func startDaemon(t *testing.T) string {
	t.Helper()
	requireBuiltArtifacts(t)

	dir := t.TempDir()
	sock := filepath.Join(dir, "sock")
	cmd, err := testdaemon.Start(combBin(), sock, 30*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { testdaemon.Stop(cmd) })
	return sock
}

// requireClient starts a fresh daemon and returns a Client wired to it.
func requireClient(t *testing.T) *beachcomber.Client {
	t.Helper()
	c, err := beachcomber.NewClient(beachcomber.WithSocketPath(startDaemon(t)), beachcomber.WithTimeoutMs(testIOTimeoutMs))
	if err != nil {
		t.Fatalf("NewClient: %v", err)
	}
	t.Cleanup(func() { c.Close() })
	return c
}
