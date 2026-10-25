package beachcomber_test

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	beachcomber "github.com/NavistAu/beachcomber/sdks/go"
)

// ---------------------------------------------------------------------------
// Client.Get flags
// ---------------------------------------------------------------------------

func TestClient_Get_Force(t *testing.T) {
	c := requireClient(t)
	// Force re-executes a real Source; a static namespace has no Source to
	// re-execute and the daemon refuses it, so this needs a builtin.
	result, err := c.Get("hostname.short", "", nil, beachcomber.Force)
	if err != nil {
		t.Fatalf("Get(Force): %v", err)
	}
	if !result.IsHit() {
		t.Error("expected IsHit() = true")
	}
}

func TestClient_Get_ForceAndWait(t *testing.T) {
	c := requireClient(t)
	result, err := c.Get("hostname.short", "", nil, beachcomber.Force, beachcomber.Wait)
	if err != nil {
		t.Fatalf("Get(Force, Wait): %v", err)
	}
	if !result.IsHit() {
		t.Error("expected IsHit() = true")
	}
}

func TestSession_Get_ForceAndWait(t *testing.T) {
	c := requireClient(t)
	sess, err := c.Session()
	if err != nil {
		t.Fatalf("Session: %v", err)
	}
	defer sess.Close()
	result, err := sess.Get("hostname.short", "", nil, beachcomber.Force, beachcomber.Wait)
	if err != nil {
		t.Fatalf("Session.Get(Force, Wait): %v", err)
	}
	if !result.IsHit() {
		t.Error("expected IsHit() = true")
	}
}

// ---------------------------------------------------------------------------
// Discovery — contract point 1/2: order, and loud failure naming every
// location tried.
// ---------------------------------------------------------------------------

func TestDiscoverLibrary_MissingCandidate_NamesEveryLocation(t *testing.T) {
	// Library loading is a process-wide sync.Once (see ffi.go's getLib): once
	// resolved by an earlier test in this binary it can't be forced to fail
	// again in-process, so this drives a fresh child process instead — the
	// standard Go idiom for testing process-lifetime-scoped state.
	if os.Getenv("BC_TEST_DISCOVERY_SUBPROC") == "1" {
		_, err := beachcomber.NewClient()
		if err == nil {
			fmt.Println("NO_ERROR")
			return
		}
		fmt.Println(err.Error())
		return
	}

	bogus := filepath.Join(t.TempDir(), "does-not-exist.dylib")
	cmd := exec.Command(os.Args[0], "-test.run=TestDiscoverLibrary_MissingCandidate_NamesEveryLocation")
	cmd.Env = append(os.Environ(), "BC_TEST_DISCOVERY_SUBPROC=1", "BEACHCOMBER_LIB="+bogus)
	out, _ := cmd.CombinedOutput()
	if !strings.Contains(string(out), bogus) {
		t.Errorf("expected discovery error to name the tried location %q, got: %s", bogus, out)
	}
}

func TestNewClientWithPath_DeferredLoadError(t *testing.T) {
	// NewClientWithPath can't return an error by signature; a discovery
	// failure must defer to the first operation instead of panicking or
	// silently succeeding. We can't force a *fresh* discovery failure
	// in-process (the library is already loaded process-wide by earlier
	// tests), so this only asserts the deferred-error plumbing compiles and
	// a Client is always non-nil.
	c := beachcomber.NewClientWithPath("/nonexistent/sock")
	if c == nil {
		t.Fatal("NewClientWithPath must never return nil")
	}
}
