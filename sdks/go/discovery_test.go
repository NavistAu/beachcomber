package beachcomber_test

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	beachcomber "github.com/NavistAu/beachcomber/sdks/go"
)

func TestDiscoverSocketPath_BeachcomberSocket(t *testing.T) {
	// BEACHCOMBER_SOCKET takes precedence over everything else.
	t.Setenv("BEACHCOMBER_SOCKET", "/custom/path/comb.sock")
	t.Setenv("XDG_RUNTIME_DIR", "/run/user/1000")
	t.Setenv("TMPDIR", "/should-not-be-used")

	got := beachcomber.DiscoverSocketPath()
	if got != "/custom/path/comb.sock" {
		t.Errorf("expected %q, got %q", "/custom/path/comb.sock", got)
	}
}

func TestDiscoverSocketPath_XDG(t *testing.T) {
	// XDG_RUNTIME_DIR resolves unconditionally (no existence probe), matching
	// where the daemon binds.
	t.Setenv("BEACHCOMBER_SOCKET", "")
	t.Setenv("XDG_RUNTIME_DIR", "/run/user/1000")
	t.Setenv("TMPDIR", "/should-not-be-used")

	got := beachcomber.DiscoverSocketPath()
	want := filepath.Join("/run/user/1000", "beachcomber", "sock")
	if got != want {
		t.Errorf("expected %q, got %q", want, got)
	}
}

func TestDiscoverSocketPath_XDGUnset(t *testing.T) {
	// XDG unset: fall through to /tmp. TMPDIR is NOT consulted.
	t.Setenv("BEACHCOMBER_SOCKET", "")
	t.Setenv("XDG_RUNTIME_DIR", "")
	t.Setenv("TMPDIR", "/should-not-be-used")

	got := beachcomber.DiscoverSocketPath()
	uid := os.Getuid()
	want := fmt.Sprintf("/tmp/beachcomber-%d/sock", uid)
	if got != want {
		t.Errorf("expected %q, got %q", want, got)
	}
}

func TestDiscoverSocketPath_TMPDIRIgnored(t *testing.T) {
	// TMPDIR must never influence resolution.
	t.Setenv("BEACHCOMBER_SOCKET", "")
	t.Setenv("XDG_RUNTIME_DIR", "")
	t.Setenv("TMPDIR", "/var/folders/xyz")

	got := beachcomber.DiscoverSocketPath()
	uid := os.Getuid()
	want := fmt.Sprintf("/tmp/beachcomber-%d/sock", uid)
	if got != want {
		t.Errorf("expected %q, got %q", want, got)
	}
}

func TestDiscoverSocketPath_ContainsUID(t *testing.T) {
	t.Setenv("BEACHCOMBER_SOCKET", "")
	t.Setenv("XDG_RUNTIME_DIR", "")

	got := beachcomber.DiscoverSocketPath()
	uid := fmt.Sprintf("%d", os.Getuid())
	if !strings.Contains(got, uid) {
		t.Errorf("expected path to contain uid %s, got %q", uid, got)
	}
}

func TestDiscoverSocketPath_EndsSock(t *testing.T) {
	t.Setenv("BEACHCOMBER_SOCKET", "")
	t.Setenv("XDG_RUNTIME_DIR", "")

	got := beachcomber.DiscoverSocketPath()
	if !strings.HasSuffix(got, "/sock") {
		t.Errorf("expected path to end with /sock, got %q", got)
	}
}
