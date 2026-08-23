package beachcomber_test

import (
	"errors"
	"testing"

	beachcomber "github.com/NavistAu/beachcomber/sdks/go"
)

// ---------------------------------------------------------------------------
// Client.Get
// ---------------------------------------------------------------------------

func TestClient_Get_Miss(t *testing.T) {
	c := requireClient(t)
	result, err := c.Get("nonexistent.provider", "")
	// An unknown provider is a daemon-side error, not a miss; assert we get
	// *some* typed outcome without crashing the binding either way.
	if err == nil && !result.IsMiss() && !result.IsHit() {
		t.Fatalf("unexpected result shape: %+v", result)
	}
}

func TestClient_Get_Hostname_Hit(t *testing.T) {
	c := requireClient(t)
	result, err := c.Get("hostname.short", "")
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	if !result.IsHit() {
		t.Fatal("expected IsHit() = true for hostname.short")
	}
	if _, ok := result.GetString(""); !ok {
		t.Error("expected hostname.short to decode as a string")
	}
}

func TestClient_Get_ServerError_HasKindAndVersion(t *testing.T) {
	c := requireClient(t)
	// A path expression evaluated against a bogus nested field path is
	// rejected server-side (see docs/protocol-spec.md).
	_, err := c.Get("hostname.short.nested.too.deep", "")
	if err == nil {
		t.Fatal("expected an error for an over-deep field path")
	}
	var se *beachcomber.ServerError
	if !errors.As(err, &se) {
		t.Fatalf("expected *ServerError, got %T: %v", err, err)
	}
	if se.Kind == "" {
		t.Error("ServerError.Kind should be populated")
	}
	if se.LibVersion == "" {
		t.Error("ServerError.LibVersion should be populated")
	}
}

// ---------------------------------------------------------------------------
// Client.Put / Refresh / Status
// ---------------------------------------------------------------------------

func TestClient_PutThenGet(t *testing.T) {
	c := requireClient(t)
	if err := c.Put("mykey", map[string]interface{}{"val": float64(42)}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	result, err := c.Get("mykey", "")
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	if !result.IsHit() {
		t.Fatal("expected IsHit() = true after Put")
	}
	if v, ok := result.GetFloat("val"); !ok || v != 42 {
		t.Errorf("val = (%v, %v), want (42, true)", v, ok)
	}
}

func TestClient_Refresh(t *testing.T) {
	c := requireClient(t)
	if err := c.Refresh("hostname", ""); err != nil {
		t.Fatalf("Refresh: %v", err)
	}
}

func TestClient_Status(t *testing.T) {
	c := requireClient(t)
	if err := c.Put("statusprobe", map[string]interface{}{"x": float64(1)}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	if _, err := c.Get("statusprobe", ""); err != nil {
		t.Fatalf("Get: %v", err)
	}
	rows, err := c.Status()
	if err != nil {
		t.Fatalf("Status: %v", err)
	}
	found := false
	for _, r := range rows {
		if r.Provider == "statusprobe" {
			found = true
		}
	}
	if !found {
		t.Errorf("statusprobe not found in Status() rows: %+v", rows)
	}
}

// ---------------------------------------------------------------------------
// Client.Hello / Introspect
// ---------------------------------------------------------------------------

func TestClient_Hello(t *testing.T) {
	c := requireClient(t)
	info, err := c.Hello()
	if err != nil {
		t.Fatalf("Hello: %v", err)
	}
	if info.ProtocolVersion == "" || info.DaemonVersion == "" {
		t.Errorf("Hello() = %+v, want non-empty versions", info)
	}
}

func TestClient_Introspect_Daemon(t *testing.T) {
	c := requireClient(t)
	resp, err := c.Introspect(beachcomber.SubjectDaemon, 0)
	if err != nil {
		t.Fatalf("Introspect: %v", err)
	}
	if resp.Daemon == nil {
		t.Fatal("Daemon is nil")
	}
	if resp.Daemon.PID == 0 {
		t.Error("Daemon.PID should be non-zero")
	}
}

func TestClient_Introspect_NonDaemon(t *testing.T) {
	c := requireClient(t)
	resp, err := c.Introspect(beachcomber.SubjectProviders, 0)
	if err != nil {
		t.Fatalf("Introspect: %v", err)
	}
	if resp.Daemon != nil {
		t.Error("Daemon should be nil for a non-daemon subject")
	}
	if resp.Other == nil {
		t.Error("Other should be populated for a non-daemon subject")
	}
}

// ---------------------------------------------------------------------------
// Client.Watch
// ---------------------------------------------------------------------------

func TestClient_Watch_NextEvent(t *testing.T) {
	c := requireClient(t)
	ws, err := c.Watch("hostname.short", "")
	if err != nil {
		t.Fatalf("Watch: %v", err)
	}
	defer ws.Close()

	ev, err := ws.NextEvent()
	if err != nil {
		t.Fatalf("NextEvent: %v", err)
	}
	if ev == nil {
		t.Fatal("expected an initial event, got nil")
	}
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

func TestSession_GetAndSetContext(t *testing.T) {
	c := requireClient(t)
	sess, err := c.Session()
	if err != nil {
		t.Fatalf("Session: %v", err)
	}
	defer sess.Close()

	if err := c.Put("sessionprobe", map[string]interface{}{"v": float64(7)}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	result, err := sess.Get("sessionprobe", "")
	if err != nil {
		t.Fatalf("Session.Get: %v", err)
	}
	if !result.IsHit() {
		t.Fatal("expected IsHit() = true")
	}

	if err := sess.SetContext("/tmp"); err != nil {
		t.Fatalf("SetContext: %v", err)
	}
}

func TestSession_Put(t *testing.T) {
	c := requireClient(t)
	sess, err := c.Session()
	if err != nil {
		t.Fatalf("Session: %v", err)
	}
	defer sess.Close()

	if err := sess.Put("sessionenv", map[string]interface{}{"color": "red"}, "", ""); err != nil {
		t.Fatalf("Session.Put: %v", err)
	}
	result, err := sess.Get("sessionenv", "")
	if err != nil {
		t.Fatalf("Session.Get: %v", err)
	}
	if s, _ := result.GetString("color"); s != "red" {
		t.Errorf("color = %q, want red", s)
	}
}

func TestSession_DelegatedOps(t *testing.T) {
	c := requireClient(t)
	sess, err := c.Session()
	if err != nil {
		t.Fatalf("Session: %v", err)
	}
	defer sess.Close()

	if _, err := sess.Hello(); err != nil {
		t.Errorf("Session.Hello: %v", err)
	}
	if _, err := sess.Status(); err != nil {
		t.Errorf("Session.Status: %v", err)
	}
	if _, err := sess.Introspect(beachcomber.SubjectDaemon, 0); err != nil {
		t.Errorf("Session.Introspect: %v", err)
	}
	if err := sess.Refresh("hostname", ""); err != nil {
		t.Errorf("Session.Refresh: %v", err)
	}
}

func TestSession_Close(t *testing.T) {
	c := requireClient(t)
	sess, err := c.Session()
	if err != nil {
		t.Fatalf("Session: %v", err)
	}
	if err := sess.Close(); err != nil {
		t.Errorf("Close: %v", err)
	}
}

// ---------------------------------------------------------------------------
// Daemon not running
// ---------------------------------------------------------------------------

func TestClient_Get_DaemonNotRunning(t *testing.T) {
	requireBuiltArtifacts(t)
	c := beachcomber.NewClientWithPath(t.TempDir() + "/nonexistent.sock")
	_, err := c.Get("git.branch", "")
	if err == nil {
		t.Fatal("expected error, got nil")
	}
	if !errors.Is(err, beachcomber.ErrDaemonNotRunning) {
		t.Errorf("expected ErrDaemonNotRunning, got %T: %v", err, err)
	}
}

// ---------------------------------------------------------------------------
// NewClient auto-discovery
// ---------------------------------------------------------------------------

func TestNewClient_AutoDiscovery(t *testing.T) {
	sock := startDaemon(t)
	t.Setenv("BEACHCOMBER_SOCKET", sock)

	c, err := beachcomber.NewClient()
	if err != nil {
		t.Fatalf("NewClient: %v", err)
	}
	result, err := c.Get("hostname.short", "")
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	if !result.IsHit() {
		t.Error("expected IsHit() = true")
	}
}

// ---------------------------------------------------------------------------
// Resolve / Eval
// ---------------------------------------------------------------------------

func TestClient_Resolve_VirtualField(t *testing.T) {
	c := requireClient(t)
	result, err := c.Resolve("filters.based", "/", map[string]string{"PYVAR": "/foo/bar/baz"},
		map[string]string{"filters.based": "env.PYVAR | basename"})
	if err != nil {
		t.Fatalf("Resolve: %v", err)
	}
	if s, ok := result.GetString(""); !ok || s != "baz" {
		t.Errorf("Resolve() = (%q, %v), want (\"baz\", true)", s, ok)
	}
}

func TestClient_Resolve_PathExpression(t *testing.T) {
	c := requireClient(t)
	result, err := c.Resolve("myproject", "/Users/x/repo-a", nil,
		map[string]string{"myproject": "'workspace-a' if cwd == '/Users/x/repo-a' else 'workspace-b'"})
	if err != nil {
		t.Fatalf("Resolve: %v", err)
	}
	if s, ok := result.GetString(""); !ok || s != "workspace-a" {
		t.Errorf("Resolve() = (%q, %v), want (\"workspace-a\", true)", s, ok)
	}
}

func TestClient_Eval(t *testing.T) {
	c := requireClient(t)
	result, err := c.Eval("env.FOO | truncate(2)", "/", map[string]string{"FOO": "hello"}, nil)
	if err != nil {
		t.Fatalf("Eval: %v", err)
	}
	if s, ok := result.GetString(""); !ok || s != "he..." {
		t.Errorf("Eval() = (%q, %v), want (\"he...\", true)", s, ok)
	}
}
