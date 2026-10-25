package beachcomber_test

import (
	"encoding/json"
	"os"
	"path/filepath"
	"reflect"
	"runtime"
	"strings"
	"testing"

	beachcomber "github.com/NavistAu/beachcomber/sdks/go"
)

// discoveryFixture is sdks/discovery.json: the library-discovery rule
// (docs/sdk-api.md, "Library discovery") as cases every SDK runs.
type discoveryFixture struct {
	Cases []struct {
		Name   string            `json:"name"`
		Tree   map[string]string `json:"tree"`
		Path   []string          `json:"path"`
		LibEnv *string           `json:"lib_env"`
		Expect []string          `json:"expect"`
	} `json:"cases"`
}

func TestLibraryDiscovery_FollowsTheSharedFixture(t *testing.T) {
	raw, err := os.ReadFile(filepath.Join("..", "discovery.json"))
	if err != nil {
		t.Fatalf("read the shared fixture: %v", err)
	}
	var fixture discoveryFixture
	if err := json.Unmarshal(raw, &fixture); err != nil {
		t.Fatalf("parse the shared fixture: %v", err)
	}
	if len(fixture.Cases) == 0 {
		t.Fatal("the shared fixture has no cases")
	}
	lib := "libbeachcomber.so"
	if runtime.GOOS == "darwin" {
		lib = "libbeachcomber.dylib"
	}

	for _, c := range fixture.Cases {
		t.Run(c.Name, func(t *testing.T) {
			dir, err := os.MkdirTemp("/tmp", "bcdisc")
			if err != nil {
				t.Fatal(err)
			}
			t.Cleanup(func() { os.RemoveAll(dir) })
			root, err := filepath.EvalSymlinks(dir)
			if err != nil {
				t.Fatal(err)
			}
			sub := strings.NewReplacer("{root}", root, "{lib}", lib).Replace

			for rel, kind := range c.Tree {
				p := filepath.Join(root, sub(rel))
				if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
					t.Fatal(err)
				}
				switch {
				case kind == "exe":
					err = os.WriteFile(p, []byte("#!/bin/sh\n"), 0o755)
				case kind == "file":
					err = os.WriteFile(p, nil, 0o644)
				case strings.HasPrefix(kind, "-> "):
					err = os.Symlink(strings.TrimPrefix(kind, "-> "), p)
				default:
					t.Fatalf("unknown tree entry %q: %q", rel, kind)
				}
				if err != nil {
					t.Fatal(err)
				}
			}

			dirs := make([]string, len(c.Path))
			for i, d := range c.Path {
				dirs[i] = sub(d)
			}
			// For this call only: t.Setenv restores both when the case ends.
			t.Setenv("PATH", strings.Join(dirs, string(os.PathListSeparator)))
			t.Setenv("BEACHCOMBER_LIB", "")
			if c.LibEnv != nil {
				os.Setenv("BEACHCOMBER_LIB", sub(*c.LibEnv))
			} else {
				os.Unsetenv("BEACHCOMBER_LIB")
			}

			want := make([]string, len(c.Expect))
			for i, e := range c.Expect {
				want[i] = sub(e)
			}
			if got := beachcomber.CandidateLibraryPathsForTest(); !reflect.DeepEqual(got, want) {
				t.Errorf("candidates = %q, want %q", got, want)
			}
		})
	}
}
