package beachcomber_test

import (
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	beachcomber "github.com/NavistAu/beachcomber/sdks/go"
)

// ---------------------------------------------------------------------------
// Client.Get options
// ---------------------------------------------------------------------------

func TestClient_Get_Force(t *testing.T) {
	c := requireClient(t)
	// Force re-executes a real Source; a static namespace has no Source to
	// re-execute and the daemon refuses it, so this needs a builtin.
	v, err := c.Get("hostname.short", "", nil, beachcomber.Force)
	if err != nil {
		t.Fatalf("Get(Force): %v", err)
	}
	if v == nil {
		t.Error("expected a value, got a miss")
	}
}

func TestClient_Get_ForceAndWait(t *testing.T) {
	c := requireClient(t)
	for _, wait := range []string{"forever", "0", "50ms"} {
		v, err := c.Get("hostname.short", "", nil, beachcomber.Force, beachcomber.Wait(wait))
		if err != nil {
			t.Fatalf("Get(Force, Wait(%q)): %v", wait, err)
		}
		if v == nil {
			t.Errorf("Wait(%q): expected a value, got a miss", wait)
		}
	}
}

func TestSession_Get_ForceAndWait(t *testing.T) {
	c := requireClient(t)
	sess, err := c.Session()
	if err != nil {
		t.Fatalf("Session: %v", err)
	}
	defer sess.Close()
	v, err := sess.Get("hostname.short", "", nil, beachcomber.Force, beachcomber.Wait("forever"))
	if err != nil {
		t.Fatalf("Session.Get(Force, Wait): %v", err)
	}
	if v == nil {
		t.Error("expected a value, got a miss")
	}
}

// The library reads the wait, so one that is no duration is its parse_error,
// on a client and a session alike.
func TestGet_AWaitThatIsNoDurationIsAParseError(t *testing.T) {
	c := requireClient(t)
	_, err := c.Get("hostname.short", "", nil, beachcomber.Wait("soon"))
	if !errors.Is(err, beachcomber.ErrParse) || !strings.Contains(err.Error(), "invalid wait `soon`") {
		t.Fatalf("Get(Wait(soon)): got %v, want a parse_error naming the wait", err)
	}
	sess, err := c.Session()
	if err != nil {
		t.Fatalf("Session: %v", err)
	}
	defer sess.Close()
	if _, err := sess.Get("hostname.short", "", nil, beachcomber.Wait("soon")); !errors.Is(err, beachcomber.ErrParse) {
		t.Fatalf("Session.Get(Wait(soon)): got %v, want a parse_error", err)
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
		fmt.Printf("library_not_found=%v %s\n", errors.Is(err, beachcomber.ErrLibraryNotFound), err.Error())
		return
	}

	bogus := filepath.Join(t.TempDir(), "does-not-exist.dylib")
	cmd := exec.Command(os.Args[0], "-test.run=TestDiscoverLibrary_MissingCandidate_NamesEveryLocation")
	cmd.Env = append(os.Environ(), "BC_TEST_DISCOVERY_SUBPROC=1", "BEACHCOMBER_LIB="+bogus)
	out, _ := cmd.CombinedOutput()
	if !strings.Contains(string(out), bogus) {
		t.Errorf("expected discovery error to name the tried location %q, got: %s", bogus, out)
	}
	if !strings.Contains(string(out), "library_not_found=true") {
		t.Errorf("expected the discovery error to be ErrLibraryNotFound, got: %s", out)
	}
	// $BEACHCOMBER_LIB set is the only candidate: nothing else is tried.
	lib := "libbeachcomber.so"
	if runtime.GOOS == "darwin" {
		lib = "libbeachcomber.dylib"
	}
	if strings.Contains(string(out), "; "+lib) || strings.Contains(string(out), "/lib/"+lib) {
		t.Errorf("expected only $BEACHCOMBER_LIB to be tried, got: %s", out)
	}
}
