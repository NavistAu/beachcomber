package beachcomber_test

import (
	"errors"
	"strings"
	"testing"

	beachcomber "github.com/NavistAu/beachcomber/sdks/go"
)

// ---------------------------------------------------------------------------
// Client.Get
// ---------------------------------------------------------------------------

func TestClient_Get_Miss(t *testing.T) {
	c := requireClient(t)
	result, err := c.Get("nonexistent.provider", "")
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	if !result.IsMiss() {
		t.Fatalf("an unknown provider is a miss, got %+v", result)
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
	// An invalid facet key (two colons) is refused by the daemon; an address
	// nothing answers, however deep, is a miss, not an error.
	_, err := c.Get("a:b:c.x", "")
	if err == nil {
		t.Fatal("expected an error for an invalid facet key")
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

func TestClient_PutNilRemovesTheNode(t *testing.T) {
	c := requireClient(t)
	if err := c.Put("gonilrm", map[string]interface{}{"val": float64(1)}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	if err := c.Put("gonilrm", nil, "", ""); err != nil {
		t.Fatalf("Put nil: %v", err)
	}
	result, err := c.Get("gonilrm.val", "")
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	if result.IsHit() {
		t.Fatal("expected a miss after Put(key, nil)")
	}
}

// The template half (design §3.4): Get returns both halves, unrendered.
func TestClient_Get_TemplateOnlyIsNeitherHitNorMiss(t *testing.T) {
	c := requireClient(t)
	if err := c.Put("tpl1:template", map[string]interface{}{"t": "{{ 1 }}"}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	result, err := c.Get("tpl1.t", "")
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	if result.Template != "{{ 1 }}" {
		t.Errorf("Template = %v, want \"{{ 1 }}\"", result.Template)
	}
	if result.Data != nil || result.IsHit() || result.IsMiss() {
		t.Errorf("template-only node: Data=%v IsHit=%v IsMiss=%v, want nil/false/false", result.Data, result.IsHit(), result.IsMiss())
	}
}

func TestClient_Get_CarriesBothHalves(t *testing.T) {
	c := requireClient(t)
	if err := c.Put("tpl2", map[string]interface{}{"v": float64(1)}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	if err := c.Put("tpl2:template", map[string]interface{}{"v": "{{ 2 }}"}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	result, err := c.Get("tpl2.v", "")
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	if !result.IsHit() {
		t.Fatal("expected IsHit() = true")
	}
	if v, ok := result.GetFloat(""); !ok || v != 1 {
		t.Errorf("Data = %v, want 1", result.Data)
	}
	if result.Template != "{{ 2 }}" {
		t.Errorf("Template = %v, want \"{{ 2 }}\"", result.Template)
	}
	if result.Rest != "" {
		t.Errorf("Rest = %q, want \"\"", result.Rest)
	}
}

func TestClient_Get_PastATemplateStringSetsRest(t *testing.T) {
	c := requireClient(t)
	if err := c.Put("tpl3:template", map[string]interface{}{"t": "{{ 1 }}"}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	result, err := c.Get("tpl3.t.x", "")
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	if result.Template != "{{ 1 }}" || result.Rest != "x" {
		t.Errorf("Template=%v Rest=%q, want \"{{ 1 }}\"/\"x\"", result.Template, result.Rest)
	}
}

func TestClient_Refresh(t *testing.T) {
	c := requireClient(t)
	if err := c.Refresh("hostname", ""); err != nil {
		t.Fatalf("Refresh: %v", err)
	}
}

// A put-created namespace has no Source to re-execute, so the daemon
// refuses; ServerError.Message is its words alone — the same text `comb
// refresh` prints — with no `server error: ` prefix from the envelope.
func TestClient_Refresh_RefusalIsTheDaemonsWords(t *testing.T) {
	c := requireClient(t)
	if err := c.Put("sourceless", map[string]interface{}{"v": float64(1)}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	err := c.Refresh("sourceless", "")
	var se *beachcomber.ServerError
	if !errors.As(err, &se) {
		t.Fatalf("expected *ServerError, got %T: %v", err, err)
	}
	if se.Kind != "server_error" {
		t.Errorf("Kind = %q, want server_error", se.Kind)
	}
	if !strings.HasPrefix(se.Message, "nothing to refresh: sourceless has no sources") {
		t.Errorf("Message = %q, want the daemon's words alone", se.Message)
	}
}

func TestClient_StatusRowSaysWhenItPollsNext(t *testing.T) {
	c := requireClient(t)
	if err := c.Put("statusttl", map[string]interface{}{"x": float64(1)}, "1m", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	rows, err := c.Status()
	if err != nil {
		t.Fatalf("Status: %v", err)
	}
	for _, r := range rows {
		if r.Provider == "statusttl" {
			if r.NextPollInSecs == nil {
				t.Errorf("a ttl row says when it polls next: %+v", r)
			}
			return
		}
	}
	t.Errorf("statusttl not found in Status() rows: %+v", rows)
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
	ws, err := c.Watch("hostname.short", "/tmp", nil)
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

// The first event is what Resolve answers for the same key, cwd and env —
// template rendered, environment applied — and a put to the template the
// value is rendered from fires the watch again.
func TestClient_Watch_ResolvedValueAndTemplateRefire(t *testing.T) {
	c := requireClient(t)
	env := map[string]string{"GO_WATCH_TPL": "v1"}
	if err := c.Put("gowatchtpl:template", map[string]interface{}{"f": "{{ env.GO_WATCH_TPL }}"}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}

	resolved, err := c.Resolve("gowatchtpl.f", "/tmp", env)
	if err != nil {
		t.Fatalf("Resolve: %v", err)
	}

	ws, err := c.Watch("gowatchtpl.f", "/tmp", env)
	if err != nil {
		t.Fatalf("Watch: %v", err)
	}
	defer ws.Close()

	ev, err := ws.NextEvent()
	if err != nil {
		t.Fatalf("NextEvent: %v", err)
	}
	if ev == nil || ev.Data != resolved.Data {
		t.Fatalf("first event %v must equal Resolve's %v", ev, resolved.Data)
	}

	if err := c.Put("gowatchtpl:template", map[string]interface{}{"f": "{{ env.GO_WATCH_TPL }}!"}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	ev, err = ws.NextEvent()
	if err != nil {
		t.Fatalf("NextEvent after template put: %v", err)
	}
	if ev == nil || ev.Data != "v1!" {
		t.Fatalf("expected the re-rendered value, got %v", ev)
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

// The daemon holds the template; the client renders it with the env the
// caller passes — the same value `comb get filters.based` prints.
func TestClient_Resolve_RendersAPutTemplate(t *testing.T) {
	c := requireClient(t)
	if err := c.Put("filters:template", map[string]interface{}{"based": "{{ env.PYVAR | basename }}"}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	result, err := c.Resolve("filters.based", "/", map[string]string{"PYVAR": "/foo/bar/baz"})
	if err != nil {
		t.Fatalf("Resolve: %v", err)
	}
	if s, ok := result.GetString(""); !ok || s != "baz" {
		t.Errorf("Resolve() = (%q, %v), want (\"baz\", true)", s, ok)
	}
}

func TestClient_Resolve_BareProviderIsTheMergedNamespace(t *testing.T) {
	c := requireClient(t)
	if err := c.Put("ns", map[string]interface{}{"lit": float64(1)}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	if err := c.Put("ns:template", map[string]interface{}{"t": "{{ env.NS }}"}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	result, err := c.Resolve("ns", "/", map[string]string{"NS": "hi"})
	if err != nil {
		t.Fatalf("Resolve: %v", err)
	}
	if v, ok := result.GetFloat("lit"); !ok || v != 1 {
		t.Errorf("lit = (%v, %v), want (1, true)", v, ok)
	}
	if s, ok := result.GetString("t"); !ok || s != "hi" {
		t.Errorf("t = (%q, %v), want (\"hi\", true)", s, ok)
	}
}

func TestClient_Resolve_MissIsNilData(t *testing.T) {
	c := requireClient(t)
	if err := c.Put("ns2", map[string]interface{}{"v": float64(1)}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	result, err := c.Resolve("ns2.nope", "/", nil)
	if err != nil {
		t.Fatalf("Resolve: %v", err)
	}
	if !result.IsMiss() {
		t.Errorf("Resolve() = %+v, want a miss", result)
	}
}

func TestClient_Eval(t *testing.T) {
	c := requireClient(t)
	result, err := c.Eval("{{ env.FOO | truncate(2) }}", "/", map[string]string{"FOO": "hello"})
	if err != nil {
		t.Fatalf("Eval: %v", err)
	}
	if s, ok := result.GetString(""); !ok || s != "he..." {
		t.Errorf("Eval() = (%q, %v), want (\"he...\", true)", s, ok)
	}
}

// ---------------------------------------------------------------------------
// WithExec
// ---------------------------------------------------------------------------

func TestWithExec_ReachesTheOptionsJSON(t *testing.T) {
	if got := beachcomber.OptionsJSONForTest("/tmp/s"); strings.Contains(got, `"exec"`) {
		t.Errorf("exec is off by default, but options_json = %s", got)
	}
	got := beachcomber.OptionsJSONForTest("/tmp/s", beachcomber.WithExec(true))
	if !strings.Contains(got, `"exec":true`) {
		t.Errorf("options_json = %s, want it to carry \"exec\":true", got)
	}
}

func TestClient_Resolve_ExecIsDisabledWithoutTheOption(t *testing.T) {
	c := requireClient(t)
	if err := c.Put("execns:template", map[string]interface{}{"out": "{{ exec(['printf', 'x']) }}"}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	_, err := c.Resolve("execns.out", "/", nil)
	var se *beachcomber.ServerError
	if !errors.As(err, &se) {
		t.Fatalf("expected *ServerError, got %T: %v", err, err)
	}
	if !strings.Contains(se.Error(), "exec() is disabled in this context") {
		t.Errorf("error = %q, want the disabled-exec message", se.Error())
	}
}

func TestClient_Resolve_ExecRunsWithTheOption(t *testing.T) {
	sock := startDaemon(t)
	c := beachcomber.NewClientWithPath(sock, beachcomber.WithExec(true))
	if err := c.Put("execns2:template", map[string]interface{}{"out": "{{ exec(['printf', 'x']) }}"}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	result, err := c.Resolve("execns2.out", "/", nil)
	if err != nil {
		t.Fatalf("Resolve: %v", err)
	}
	if s, ok := result.GetString(""); !ok || s != "x" {
		t.Errorf("Resolve() = (%q, %v), want (\"x\", true)", s, ok)
	}
}
