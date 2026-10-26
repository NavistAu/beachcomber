package beachcomber_test

// The SDK API table (docs/sdk-api.md) in its machine-readable form,
// ../api.json, checked against this package's public surface: the method
// sets, the client options, the error kinds and the typed shapes. An SDK
// that drifts from the table fails here.

import (
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"reflect"
	"sort"
	"strings"
	"testing"

	beachcomber "github.com/NavistAu/beachcomber/sdks/go"
)

type apiTable struct {
	Client struct {
		Options []string `json:"options"`
		Methods []string `json:"methods"`
	} `json:"client"`
	Session struct {
		Methods []string `json:"methods"`
	} `json:"session"`
	WatchStream struct {
		Methods []string `json:"methods"`
	} `json:"watch_stream"`
	Types       map[string]json.RawMessage `json:"types"`
	ErrorFields []string                   `json:"error_fields"`
	ErrorKinds  []string                   `json:"error_kinds"`
}

func loadAPITable(t *testing.T) apiTable {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join("..", "api.json"))
	if err != nil {
		t.Fatalf("read the API table: %v", err)
	}
	var table apiTable
	if err := json.Unmarshal(raw, &table); err != nil {
		t.Fatalf("parse the API table: %v", err)
	}
	return table
}

// goInitialisms are the table's snake segments Go spells as initialisms.
var goInitialisms = map[string]string{"pid": "PID"}

// pascal spells a table name the way Go exports it: each snake segment
// capitalised, initialisms in capitals.
func pascal(snake string) string {
	var b strings.Builder
	for _, seg := range strings.Split(snake, "_") {
		if up, ok := goInitialisms[seg]; ok {
			b.WriteString(up)
			continue
		}
		b.WriteString(strings.ToUpper(seg[:1]) + seg[1:])
	}
	return b.String()
}

func pascalAll(names []string) []string {
	out := make([]string, len(names))
	for i, n := range names {
		out[i] = pascal(n)
	}
	sort.Strings(out)
	return out
}

func exportedMethods(v interface{}) []string {
	typ := reflect.TypeOf(v)
	var out []string
	for i := 0; i < typ.NumMethod(); i++ {
		out = append(out, typ.Method(i).Name)
	}
	sort.Strings(out)
	return out
}

func exportedFields(v interface{}) []string {
	typ := reflect.TypeOf(v)
	var out []string
	for i := 0; i < typ.NumField(); i++ {
		if f := typ.Field(i); f.IsExported() {
			out = append(out, f.Name)
		}
	}
	sort.Strings(out)
	return out
}

// reaperStatus is the zero value of the type DaemonHealth.Reaper points at,
// which the table names reaper_status.
func reaperStatus() interface{} {
	field, _ := reflect.TypeOf(beachcomber.DaemonHealth{}).FieldByName("Reaper")
	return reflect.Zero(field.Type.Elem()).Interface()
}

func TestAPI_ReaperStatusIsNamedForTheTable(t *testing.T) {
	if got := reflect.TypeOf(reaperStatus()).Name(); got != "ReaperStatus" {
		t.Errorf("DaemonHealth.Reaper is a *%s, want *ReaperStatus", got)
	}
}

func TestAPI_MethodSetsAreTheTables(t *testing.T) {
	table := loadAPITable(t)
	for _, c := range []struct {
		what  string
		value interface{}
		want  []string
	}{
		{"*Client", (*beachcomber.Client)(nil), table.Client.Methods},
		{"*Session", (*beachcomber.Session)(nil), table.Session.Methods},
		{"*WatchStream", (*beachcomber.WatchStream)(nil), table.WatchStream.Methods},
	} {
		got, want := exportedMethods(c.value), pascalAll(c.want)
		if !reflect.DeepEqual(got, want) {
			t.Errorf("%s methods = %v, want %v", c.what, got, want)
		}
	}
}

func TestAPI_TypedShapesAreTheTables(t *testing.T) {
	table := loadAPITable(t)
	shapes := map[string]interface{}{
		"cache_row":           beachcomber.CacheRow{},
		"hello_info":          beachcomber.HelloInfo{},
		"daemon_health":       beachcomber.DaemonHealth{},
		"reaper_status":       reaperStatus(),
		"verdict":             beachcomber.Verdict{},
		"introspect_response": beachcomber.IntrospectResponse{},
		"watch_event":         beachcomber.WatchEvent{},
	}
	for name, raw := range table.Types {
		var fields []string
		if json.Unmarshal(raw, &fields) != nil {
			continue // the table's own description
		}
		shape, ok := shapes[name]
		if !ok {
			t.Errorf("the table names a type %q this SDK has no shape for", name)
			continue
		}
		if got, want := exportedFields(shape), pascalAll(fields); !reflect.DeepEqual(got, want) {
			t.Errorf("%T fields = %v, want %v", shape, got, want)
		}
	}
}

// Each client option the table names has one Option constructor, writing
// exactly that key of the options_json bc_client_new takes.
func TestAPI_EachClientOptionHasItsConstructor(t *testing.T) {
	table := loadAPITable(t)
	constructors := map[string]beachcomber.Option{
		"socket_path":        beachcomber.WithSocketPath("/tmp/s"),
		"timeout_ms":         beachcomber.WithTimeoutMs(250),
		"connect_timeout_ms": beachcomber.WithConnectTimeoutMs(0),
		"autostart":          beachcomber.WithAutostart(false),
		"exec":               beachcomber.WithExec(true),
	}
	for _, name := range table.Client.Options {
		option, ok := constructors[name]
		if !ok {
			t.Errorf("the table names a client option %q this SDK has no constructor for", name)
			continue
		}
		if got := sortedKeys(optionKeys(t, option)); !reflect.DeepEqual(got, []string{name}) {
			t.Errorf("%s's constructor writes %v, want [%s]", name, got, name)
		}
		delete(constructors, name)
	}
	for name := range constructors {
		t.Errorf("this SDK has a constructor for %q, an option the table does not name", name)
	}
}

// One error type, with the table's fields, and one sentinel per kind that
// errors.Is matches on an *Error of that kind and no other.
func TestAPI_ErrorKindsAreTheTables(t *testing.T) {
	table := loadAPITable(t)
	if got, want := exportedFields(beachcomber.Error{}), pascalAll(table.ErrorFields); !reflect.DeepEqual(got, want) {
		t.Errorf("Error fields = %v, want %v", got, want)
	}
	sentinels := map[string]error{
		"daemon_not_running": beachcomber.ErrDaemonNotRunning,
		"connection_failed":  beachcomber.ErrConnectionFailed,
		"io_error":           beachcomber.ErrIo,
		"parse_error":        beachcomber.ErrParse,
		"server_error":       beachcomber.ErrServer,
		"timeout":            beachcomber.ErrTimeout,
		"busy":               beachcomber.ErrBusy,
		"panic":              beachcomber.ErrPanic,
		"version_skew":       beachcomber.ErrVersionSkew,
		"library_not_found":  beachcomber.ErrLibraryNotFound,
		"library_symbol":     beachcomber.ErrLibrarySymbol,
		"unsupported":        beachcomber.ErrUnsupported,
	}
	if len(sentinels) != len(table.ErrorKinds) {
		t.Errorf("%d sentinels, the table has %d kinds", len(sentinels), len(table.ErrorKinds))
	}
	for _, kind := range table.ErrorKinds {
		sentinel, ok := sentinels[kind]
		if !ok {
			t.Errorf("no sentinel for the table's kind %q", kind)
			continue
		}
		err := error(&beachcomber.Error{Kind: kind, Message: "m"})
		for other, s := range sentinels {
			if got := errors.Is(err, s); got != (other == kind) {
				t.Errorf("errors.Is(*Error{Kind: %q}, sentinel of %q) = %v", kind, other, got)
			}
		}
		if e, ok := sentinel.(*beachcomber.Error); !ok || e.Kind != kind {
			t.Errorf("the %q sentinel is %#v, want an *Error of that kind", kind, sentinel)
		}
	}
}
