package beachcomber_test

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	beachcomber "github.com/jhogendorn/beachcomber/sdks/go"
)

func TestDiscoverSocketPath_XDG(t *testing.T) {
	dir := t.TempDir()
	sockDir := filepath.Join(dir, "beachcomber")
	if err := os.MkdirAll(sockDir, 0o700); err != nil {
		t.Fatal(err)
	}
	sockPath := filepath.Join(sockDir, "sock")
	// Create the socket file so stat succeeds.
	f, err := os.Create(sockPath)
	if err != nil {
		t.Fatal(err)
	}
	f.Close()

	t.Setenv("XDG_RUNTIME_DIR", dir)
	t.Setenv("TMPDIR", "/should-not-be-used")

	got := beachcomber.DiscoverSocketPath()
	if got != sockPath {
		t.Errorf("expected %q, got %q", sockPath, got)
	}
}

func TestDiscoverSocketPath_XDGMissing(t *testing.T) {
	// XDG is set but the path does not exist — fall through to TMPDIR.
	dir := t.TempDir()
	tmpdir := t.TempDir()

	t.Setenv("XDG_RUNTIME_DIR", dir) // dir exists but beachcomber/sock does not
	t.Setenv("TMPDIR", tmpdir)

	got := beachcomber.DiscoverSocketPath()
	uid := os.Getuid()
	want := filepath.Join(tmpdir, fmt.Sprintf("beachcomber-%d", uid), "sock")
	if got != want {
		t.Errorf("expected %q, got %q", want, got)
	}
}

func TestDiscoverSocketPath_XDGUnset(t *testing.T) {
	tmpdir := t.TempDir()

	t.Setenv("XDG_RUNTIME_DIR", "")
	t.Setenv("TMPDIR", tmpdir)

	got := beachcomber.DiscoverSocketPath()
	uid := os.Getuid()
	want := filepath.Join(tmpdir, fmt.Sprintf("beachcomber-%d", uid), "sock")
	if got != want {
		t.Errorf("expected %q, got %q", want, got)
	}
}

func TestDiscoverSocketPath_NoTMPDIR(t *testing.T) {
	t.Setenv("XDG_RUNTIME_DIR", "")
	t.Setenv("TMPDIR", "")

	got := beachcomber.DiscoverSocketPath()
	uid := os.Getuid()
	want := fmt.Sprintf("/tmp/beachcomber-%d/sock", uid)
	if got != want {
		t.Errorf("expected %q, got %q", want, got)
	}
}

func TestDiscoverSocketPath_ContainsUID(t *testing.T) {
	t.Setenv("XDG_RUNTIME_DIR", "")
	t.Setenv("TMPDIR", "/tmp")

	got := beachcomber.DiscoverSocketPath()
	uid := fmt.Sprintf("%d", os.Getuid())
	if !strings.Contains(got, uid) {
		t.Errorf("expected path to contain uid %s, got %q", uid, got)
	}
}

func TestDiscoverSocketPath_EndsSock(t *testing.T) {
	t.Setenv("XDG_RUNTIME_DIR", "")
	t.Setenv("TMPDIR", "/tmp")

	got := beachcomber.DiscoverSocketPath()
	if !strings.HasSuffix(got, "/sock") {
		t.Errorf("expected path to end with /sock, got %q", got)
	}
}
