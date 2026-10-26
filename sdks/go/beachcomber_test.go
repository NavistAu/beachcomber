package beachcomber_test

import (
	"errors"
	"os"
	"reflect"
	"strings"
	"testing"
	"time"

	beachcomber "github.com/NavistAu/beachcomber/sdks/go"
)

// ---------------------------------------------------------------------------
// Client.Get
// ---------------------------------------------------------------------------

func TestClient_Get_Miss(t *testing.T) {
	c := requireClient(t)
	v, err := c.Get("nonexistent.provider", "", nil)
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	if v != nil {
		t.Fatalf("an unknown provider is a miss (nil), got %#v", v)
	}
}

func TestClient_Get_Hostname_Hit(t *testing.T) {
	c := requireClient(t)
	v, err := c.Get("hostname.short", "", nil)
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	if _, ok := v.(string); !ok {
		t.Errorf("hostname.short = %#v, want the bare string", v)
	}
}

// A failure is an *Error whose message is the library's words alone: the
// kind and the library's version are fields of their own.
func TestClient_Get_ErrorHasKindAndVersionApartFromItsMessage(t *testing.T) {
	c := requireClient(t)
	// An invalid facet key (two colons) is refused by the daemon; an address
	// nothing answers, however deep, is a miss, not an error.
	_, err := c.Get("a:b:c.x", "", nil)
	var e *beachcomber.Error
	if !errors.As(err, &e) {
		t.Fatalf("expected *Error, got %T: %v", err, err)
	}
	if e.Kind == "" || e.LibraryVersion == "" {
		t.Errorf("Kind = %q, LibraryVersion = %q, want both set", e.Kind, e.LibraryVersion)
	}
	if err.Error() != e.Message || strings.Contains(err.Error(), e.LibraryVersion) {
		t.Errorf("Error() = %q, want the message %q alone", err.Error(), e.Message)
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
	v, err := c.Get("mykey", "", nil)
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	if want := map[string]interface{}{"val": float64(42)}; !reflect.DeepEqual(v, want) {
		t.Errorf("Get() = %#v, want %#v", v, want)
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
	v, err := c.Get("gonilrm.val", "", nil)
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	if v != nil {
		t.Fatalf("expected a miss after Put(key, nil), got %#v", v)
	}
}

// Get is the value `comb get` prints: a template-only node renders, with the
// caller's environment.
func TestClient_Get_RendersAPutTemplateWithTheCallersEnv(t *testing.T) {
	c := requireClient(t)
	if err := c.Put("filters:template", map[string]interface{}{"based": "{{ env.PYVAR | basename }}"}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	v, err := c.Get("filters.based", "", map[string]string{"PYVAR": "/foo/bar/baz"})
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	if v != "baz" {
		t.Errorf("Get() = %#v, want \"baz\"", v)
	}
}

// A template wins over the cached value it shares an address with; the
// unrendered halves are facets, read like any key.
func TestClient_Get_FacetsReadTheUnrenderedHalves(t *testing.T) {
	c := requireClient(t)
	if err := c.Put("tpl2", map[string]interface{}{"v": float64(1)}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	if err := c.Put("tpl2:template", map[string]interface{}{"v": "{{ 2 }}"}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	for key, want := range map[string]interface{}{
		"tpl2.v":          float64(2),
		"tpl2:template.v": "{{ 2 }}",
		"tpl2:put.v":      float64(1),
	} {
		v, err := c.Get(key, "", nil)
		if err != nil {
			t.Fatalf("Get(%q): %v", key, err)
		}
		if v != want {
			t.Errorf("Get(%q) = %#v, want %#v", key, v, want)
		}
	}
}

// An address past a template string walks into what the string renders to.
func TestClient_Get_PastATemplateStringWalksIntoItsValue(t *testing.T) {
	c := requireClient(t)
	if err := c.Put("tpl3:template", map[string]interface{}{"t": "{{ {'x': 5} }}"}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	v, err := c.Get("tpl3.t.x", "", nil)
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	if v != float64(5) {
		t.Errorf("Get() = %#v, want 5", v)
	}
}

// cwd is the directory the read addresses; "" is the global entry.
func TestClient_Get_ReadsAtCwd(t *testing.T) {
	c := requireClient(t)
	if err := c.Put("gocwd", map[string]interface{}{"v": "global"}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	if err := c.Put("gocwd", map[string]interface{}{"v": "tmp"}, "", "/tmp"); err != nil {
		t.Fatalf("Put: %v", err)
	}
	for cwd, want := range map[string]string{"": "global", "/tmp": "tmp"} {
		v, err := c.Get("gocwd.v", cwd, nil)
		if err != nil {
			t.Fatalf("Get at %q: %v", cwd, err)
		}
		if v != want {
			t.Errorf("Get at %q = %#v, want %q", cwd, v, want)
		}
	}
}

func TestClient_Refresh(t *testing.T) {
	c := requireClient(t)
	if err := c.Refresh("hostname", ""); err != nil {
		t.Fatalf("Refresh: %v", err)
	}
}

// A put-created namespace has no Source to re-execute, so the daemon
// refuses; Error.Message is its words alone — the same text `comb
// refresh` prints — with no `server error: ` prefix from the envelope.
func TestClient_Refresh_RefusalIsTheDaemonsWords(t *testing.T) {
	c := requireClient(t)
	if err := c.Put("sourceless", map[string]interface{}{"v": float64(1)}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	err := c.Refresh("sourceless", "")
	var e *beachcomber.Error
	if !errors.As(err, &e) {
		t.Fatalf("expected *Error, got %T: %v", err, err)
	}
	if !errors.Is(err, beachcomber.ErrServer) {
		t.Errorf("Kind = %q, want server_error", e.Kind)
	}
	if !strings.HasPrefix(e.Message, "nothing to refresh: sourceless has no sources") {
		t.Errorf("Message = %q, want the daemon's words alone", e.Message)
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
	if _, err := c.Get("statusprobe", "", nil); err != nil {
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
	resp, err := c.Introspect(beachcomber.SubjectDaemon)
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
	resp, err := c.Introspect(beachcomber.SubjectProviders)
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

// A duration given is sent, 0 included: a procs sample asked for 0 seconds
// answers at once, where the daemon's own default samples for 2. Whether the
// sample itself succeeds depends on the host (macOS needs eslogger's
// privileges), so only a transport failure is one here.
func TestClient_Introspect_AnExplicitZeroDurationIsSent(t *testing.T) {
	c := requireClient(t)
	started := time.Now()
	_, err := c.Introspect(beachcomber.SubjectProcs, 0)
	took := time.Since(started)
	if err != nil && !errors.Is(err, beachcomber.ErrServer) {
		t.Fatalf("Introspect: %v", err)
	}
	if took >= time.Second {
		t.Errorf("a 0s procs sample took %v: the 0 was not sent", took)
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

// The first event is what Get answers for the same key, cwd and env —
// template rendered, environment applied — and a put to the template the
// value is rendered from fires the watch again.
func TestClient_Watch_ResolvedValueAndTemplateRefire(t *testing.T) {
	c := requireClient(t)
	env := map[string]string{"GO_WATCH_TPL": "v1"}
	if err := c.Put("gowatchtpl:template", map[string]interface{}{"f": "{{ env.GO_WATCH_TPL }}"}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}

	got, err := c.Get("gowatchtpl.f", "/tmp", env)
	if err != nil {
		t.Fatalf("Get: %v", err)
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
	if ev == nil || ev.Data != got {
		t.Fatalf("first event %v must equal Get's %v", ev, got)
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

// NextEvent(ms) that runs out with no event is a timeout, not the end of
// the stream; Cancel ends it, and NextEvent then answers nil, nil.
func TestWatchStream_TimeoutIsAnErrorAndCancelEndsTheStream(t *testing.T) {
	c := requireClient(t)
	ws, err := c.Watch("hostname.short", "", nil)
	if err != nil {
		t.Fatalf("Watch: %v", err)
	}
	defer ws.Close()
	if ev, err := ws.NextEvent(); err != nil || ev == nil {
		t.Fatalf("first NextEvent = %v, %v; want the initial event", ev, err)
	}

	ev, err := ws.NextEvent(50)
	if !errors.Is(err, beachcomber.ErrTimeout) {
		t.Fatalf("NextEvent(50) = %v, %v; want a timeout error", ev, err)
	}

	if err := ws.Cancel(); err != nil {
		t.Fatalf("Cancel: %v", err)
	}
	ev, err = ws.NextEvent()
	if ev != nil || err != nil {
		t.Errorf("after Cancel, NextEvent = %v, %v; want nil, nil", ev, err)
	}
}

// Watch and Eval read cwd "" as Get does: the global entry, not this
// process's working directory.
func TestClient_EvalAndWatch_NoCwdIsTheGlobalEntry(t *testing.T) {
	c := requireClient(t)
	if err := c.Put("gonocwd", map[string]interface{}{"v": "global"}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	if err := c.Put("gonocwd", map[string]interface{}{"v": "tmp"}, "", "/tmp"); err != nil {
		t.Fatalf("Put: %v", err)
	}
	// This process's working directory is where cwd "" would read if it were
	// sent as an empty path rather than NULL.
	wd, err := os.Getwd()
	if err != nil {
		t.Fatal(err)
	}
	if err := os.Chdir("/tmp"); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.Chdir(wd) })

	v, err := c.Eval("{{ gonocwd.v }}", "", nil)
	if err != nil || v != "global" {
		t.Errorf("Eval at cwd \"\" = %#v, %v; want \"global\"", v, err)
	}
	ws, err := c.Watch("gonocwd.v", "", nil)
	if err != nil {
		t.Fatalf("Watch: %v", err)
	}
	defer ws.Close()
	if ev, err := ws.NextEvent(); err != nil || ev == nil || ev.Data != "global" {
		t.Errorf("Watch at cwd \"\" first event = %v, %v; want \"global\"", ev, err)
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
	v, err := sess.Get("sessionprobe", "", nil)
	if err != nil {
		t.Fatalf("Session.Get: %v", err)
	}
	if want := map[string]interface{}{"v": float64(7)}; !reflect.DeepEqual(v, want) {
		t.Fatalf("Session.Get = %#v, want %#v", v, want)
	}

	if err := sess.SetContext("/tmp"); err != nil {
		t.Fatalf("SetContext: %v", err)
	}
}

// Session.Get is Client.Get on the session's connection: a template renders
// with the call's environment, at the session's directory once SetContext
// has set one, and at the global entry before.
func TestSession_Get_RendersAtTheSessionsDirectory(t *testing.T) {
	c := requireClient(t)
	sess, err := c.Session()
	if err != nil {
		t.Fatalf("Session: %v", err)
	}
	defer sess.Close()

	if err := sess.Put("gors:template", map[string]interface{}{"t": "{{ env.WHO }}@{{ gors.where }}"}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	if err := sess.Put("gors", map[string]interface{}{"where": "global"}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	if err := sess.Put("gors", map[string]interface{}{"where": "tmp"}, "", "/tmp"); err != nil {
		t.Fatalf("Put: %v", err)
	}
	env := map[string]string{"WHO": "me"}

	v, err := sess.Get("gors.t", "", env)
	if err != nil {
		t.Fatalf("Session.Get: %v", err)
	}
	if v != "me@global" {
		t.Errorf("before SetContext: %#v, want \"me@global\"", v)
	}

	if err := sess.SetContext("/tmp"); err != nil {
		t.Fatalf("SetContext: %v", err)
	}
	v, err = sess.Get("gors.t", "", env)
	if err != nil {
		t.Fatalf("Session.Get: %v", err)
	}
	if v != "me@tmp" {
		t.Errorf("after SetContext: %#v, want \"me@tmp\"", v)
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
	v, err := sess.Get("sessionenv.color", "", nil)
	if err != nil {
		t.Fatalf("Session.Get: %v", err)
	}
	if v != "red" {
		t.Errorf("color = %#v, want red", v)
	}
}

func TestSession_Refresh(t *testing.T) {
	c := requireClient(t)
	sess, err := c.Session()
	if err != nil {
		t.Fatalf("Session: %v", err)
	}
	defer sess.Close()

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
	if err := sess.Close(); err != nil {
		t.Errorf("a second Close: %v", err)
	}
	if _, err := sess.Get("hostname.short", "", nil); !errors.Is(err, beachcomber.ErrIo) {
		t.Errorf("Get on a closed session = %v, want an io_error", err)
	}
}

// Close frees the client: Close again is a no-op, and an operation after it
// fails rather than reaching the library with a freed handle.
func TestClient_Close(t *testing.T) {
	c := requireClient(t)
	if err := c.Close(); err != nil {
		t.Errorf("Close: %v", err)
	}
	if err := c.Close(); err != nil {
		t.Errorf("a second Close: %v", err)
	}
	if _, err := c.Get("hostname.short", "", nil); !errors.Is(err, beachcomber.ErrIo) {
		t.Errorf("Get on a closed client = %v, want an io_error", err)
	}
}

// ---------------------------------------------------------------------------
// Daemon not running
// ---------------------------------------------------------------------------

// An explicit socket nobody serves is connection_failed, its own kind, not
// folded into daemon_not_running.
func TestClient_Get_AnExplicitSocketNobodyServesIsConnectionFailed(t *testing.T) {
	requireBuiltArtifacts(t)
	c, err := beachcomber.NewClient(
		beachcomber.WithSocketPath(t.TempDir()+"/nonexistent.sock"),
		beachcomber.WithConnectTimeoutMs(0),
	)
	if err != nil {
		t.Fatalf("NewClient: %v", err)
	}
	_, err = c.Get("git.branch", "", nil)
	if !errors.Is(err, beachcomber.ErrConnectionFailed) {
		t.Errorf("expected ErrConnectionFailed, got %T: %v", err, err)
	}
	if errors.Is(err, beachcomber.ErrDaemonNotRunning) {
		t.Errorf("connection_failed must not match ErrDaemonNotRunning: %v", err)
	}
}

// ---------------------------------------------------------------------------
// NewClient auto-discovery
// ---------------------------------------------------------------------------

func TestNewClient_AutoDiscovery(t *testing.T) {
	sock := startDaemon(t)
	t.Setenv("BEACHCOMBER_SOCKET", sock)

	c, err := beachcomber.NewClient(beachcomber.WithTimeoutMs(testIOTimeoutMs))
	if err != nil {
		t.Fatalf("NewClient: %v", err)
	}
	v, err := c.Get("hostname.short", "", nil)
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	if v == nil {
		t.Error("expected a value, got a miss")
	}
}

// ---------------------------------------------------------------------------
// Get / Eval
// ---------------------------------------------------------------------------

func TestClient_Get_BareProviderIsTheMergedNamespace(t *testing.T) {
	c := requireClient(t)
	if err := c.Put("ns", map[string]interface{}{"lit": float64(1)}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	if err := c.Put("ns:template", map[string]interface{}{"t": "{{ env.NS }}"}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	v, err := c.Get("ns", "/", map[string]string{"NS": "hi"})
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	if want := map[string]interface{}{"lit": float64(1), "t": "hi"}; !reflect.DeepEqual(v, want) {
		t.Errorf("Get() = %#v, want %#v", v, want)
	}
}

func TestClient_Get_MissIsNilData(t *testing.T) {
	c := requireClient(t)
	if err := c.Put("ns2", map[string]interface{}{"v": float64(1)}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	v, err := c.Get("ns2.nope", "/", nil)
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	if v != nil {
		t.Errorf("Get() = %#v, want a miss", v)
	}
}

func TestClient_Eval(t *testing.T) {
	c := requireClient(t)
	v, err := c.Eval("{{ env.FOO | truncate(2) }}", "/", map[string]string{"FOO": "hello"})
	if err != nil {
		t.Fatalf("Eval: %v", err)
	}
	if v != "h…" {
		t.Errorf("Eval() = %#v, want \"h…\"", v)
	}
}

// ---------------------------------------------------------------------------
// WithExec
// ---------------------------------------------------------------------------

func TestWithExec_ReachesTheOptionsJSON(t *testing.T) {
	if got := beachcomber.OptionsJSONForTest(beachcomber.WithSocketPath("/tmp/s")); strings.Contains(got, `"exec"`) {
		t.Errorf("exec is off by default, but options_json = %s", got)
	}
	got := beachcomber.OptionsJSONForTest(beachcomber.WithSocketPath("/tmp/s"), beachcomber.WithExec(true))
	if !strings.Contains(got, `"exec":true`) {
		t.Errorf("options_json = %s, want it to carry \"exec\":true", got)
	}
}

func TestClient_Get_ExecIsDisabledWithoutTheOption(t *testing.T) {
	c := requireClient(t)
	if err := c.Put("execns:template", map[string]interface{}{"out": "{{ exec(['printf', 'x']) }}"}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	_, err := c.Get("execns.out", "/", nil)
	var e *beachcomber.Error
	if !errors.As(err, &e) {
		t.Fatalf("expected *Error, got %T: %v", err, err)
	}
	if !strings.Contains(e.Error(), "exec() is disabled in this context") {
		t.Errorf("error = %q, want the disabled-exec message", e.Error())
	}
}

func TestClient_Get_ExecRunsWithTheOption(t *testing.T) {
	sock := startDaemon(t)
	c, err := beachcomber.NewClient(beachcomber.WithSocketPath(sock), beachcomber.WithExec(true), beachcomber.WithTimeoutMs(testIOTimeoutMs))
	if err != nil {
		t.Fatalf("NewClient: %v", err)
	}
	if err := c.Put("execns2:template", map[string]interface{}{"out": "{{ exec(['printf', 'x']) }}"}, "", ""); err != nil {
		t.Fatalf("Put: %v", err)
	}
	v, err := c.Get("execns2.out", "/", nil)
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	if v != "x" {
		t.Errorf("Get() = %#v, want \"x\"", v)
	}
}
