package beachcomber_test

import (
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"reflect"
	"sort"
	"testing"
	"time"

	beachcomber "github.com/NavistAu/beachcomber/sdks/go"
)

// optionKeys is the set of keys the options_json a client hands
// bc_client_new carries.
func optionKeys(t *testing.T, options ...beachcomber.Option) map[string]interface{} {
	t.Helper()
	var m map[string]interface{}
	if err := json.Unmarshal([]byte(beachcomber.OptionsJSONForTest(options...)), &m); err != nil {
		t.Fatalf("options_json is not an object: %v", err)
	}
	return m
}

func sortedKeys(m map[string]interface{}) []string {
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	return keys
}

// The timeouts are the ABI's options, passed through under its spelling: a
// client given none sends none (the ABI's defaults apply), and one given
// both sends exactly those.
func TestTimeoutOptions_PassThroughUnderTheABIsSpelling(t *testing.T) {
	if got := optionKeys(t); len(got) != 0 {
		t.Errorf("no options, but options_json carries %v", sortedKeys(got))
	}
	got := optionKeys(t, beachcomber.WithTimeoutMs(250), beachcomber.WithConnectTimeoutMs(0))
	want := map[string]interface{}{"timeout_ms": float64(250), "connect_timeout_ms": float64(0)}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("options_json = %v, want %v", got, want)
	}
}

// A connect timeout of 0 is one attempt: a socket nobody serves fails at
// once with connection_failed, rather than after the 1500ms default.
func TestClient_ConnectTimeoutMs_ZeroIsOneAttempt(t *testing.T) {
	requireBuiltArtifacts(t)
	dir, err := os.MkdirTemp("/tmp", "bcgo")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(dir) })
	c, err := beachcomber.NewClient(beachcomber.WithSocketPath(filepath.Join(dir, "sock")),
		beachcomber.WithAutostart(false), beachcomber.WithConnectTimeoutMs(0))
	if err != nil {
		t.Fatalf("NewClient: %v", err)
	}

	started := time.Now()
	_, err = c.Hello()
	took := time.Since(started)

	if !errors.Is(err, beachcomber.ErrConnectionFailed) {
		t.Fatalf("Hello() error = %v, want kind connection_failed", err)
	}
	if took >= 250*time.Millisecond {
		t.Errorf("one attempt took %v", took)
	}
}
