package beachcomber_test

import (
	"testing"

	beachcomber "github.com/NavistAu/beachcomber/sdks/go"
)

// ---------------------------------------------------------------------------
// Client.GetWithFlags
// ---------------------------------------------------------------------------

func TestClient_GetWithFlags_ForceAndWait(t *testing.T) {
	h, reqs := fixedHandler(`{"ok":true,"data":"main","age_ms":5}`)
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	result, err := c.GetWithFlags("git.branch", "/repo", true, true)
	if err != nil {
		t.Fatalf("GetWithFlags: %v", err)
	}
	if !result.IsHit() {
		t.Error("expected IsHit() = true")
	}

	req := reqs()[0]
	if req["op"] != "get" {
		t.Errorf("op = %v, want get", req["op"])
	}
	if req["force"] != true {
		t.Errorf("force = %v, want true", req["force"])
	}
	if req["wait"] != true {
		t.Errorf("wait = %v, want true", req["wait"])
	}
	if req["path"] != "/repo" {
		t.Errorf("path = %v, want /repo", req["path"])
	}
}

func TestClient_GetWithFlags_NoFlags(t *testing.T) {
	h, reqs := fixedHandler(`{"ok":true,"data":"val"}`)
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	_, err := c.GetWithFlags("git.branch", "", false, false)
	if err != nil {
		t.Fatalf("GetWithFlags: %v", err)
	}

	req := reqs()[0]
	if _, hasForce := req["force"]; hasForce {
		t.Error("force should be absent when false")
	}
	if _, hasWait := req["wait"]; hasWait {
		t.Error("wait should be absent when false")
	}
	if _, hasPath := req["path"]; hasPath {
		t.Error("path should be absent when empty")
	}
}

// ---------------------------------------------------------------------------
// Client.Hello
// ---------------------------------------------------------------------------

func TestClient_Hello(t *testing.T) {
	h, reqs := fixedHandler(`{"ok":true,"data":{"protocol_version":"1","daemon_version":"0.5.0"}}`)
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	info, err := c.Hello()
	if err != nil {
		t.Fatalf("Hello: %v", err)
	}

	if info.ProtocolVersion != "1" {
		t.Errorf("ProtocolVersion = %q, want 1", info.ProtocolVersion)
	}
	if info.DaemonVersion != "0.5.0" {
		t.Errorf("DaemonVersion = %q, want 0.5.0", info.DaemonVersion)
	}
	if reqs()[0]["op"] != "hello" {
		t.Errorf("op = %v, want hello", reqs()[0]["op"])
	}
}

func TestClient_Hello_MissingVersions(t *testing.T) {
	h, _ := fixedHandler(`{"ok":true,"data":{}}`)
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	_, err := c.Hello()
	if err == nil {
		t.Fatal("expected error when versions missing, got nil")
	}
}

func TestClient_Hello_NotObject(t *testing.T) {
	h, _ := fixedHandler(`{"ok":true,"data":"unexpected"}`)
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	_, err := c.Hello()
	if err == nil {
		t.Fatal("expected error when data not object, got nil")
	}
}

// ---------------------------------------------------------------------------
// Client.Put
// ---------------------------------------------------------------------------

func TestClient_Put(t *testing.T) {
	h, reqs := fixedHandler(`{"ok":true}`)
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	err := c.Put("my.key", map[string]interface{}{"val": 42}, "", "")
	if err != nil {
		t.Fatalf("Put: %v", err)
	}

	req := reqs()[0]
	if req["op"] != "put" {
		t.Errorf("op = %v, want put", req["op"])
	}
	if req["key"] != "my.key" {
		t.Errorf("key = %v, want my.key", req["key"])
	}
	if req["data"] == nil {
		t.Error("data field should be present")
	}
	if _, hasTTL := req["ttl"]; hasTTL {
		t.Error("ttl should be absent when empty")
	}
	if _, hasPath := req["path"]; hasPath {
		t.Error("path should be absent when empty")
	}
}

func TestClient_Put_WithTTLAndPath(t *testing.T) {
	h, reqs := fixedHandler(`{"ok":true}`)
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	err := c.Put("my.key", map[string]interface{}{"x": 1}, "5m", "/repo")
	if err != nil {
		t.Fatalf("Put: %v", err)
	}

	req := reqs()[0]
	if req["ttl"] != "5m" {
		t.Errorf("ttl = %v, want 5m", req["ttl"])
	}
	if req["path"] != "/repo" {
		t.Errorf("path = %v, want /repo", req["path"])
	}
}

// ---------------------------------------------------------------------------
// Client.Introspect — daemon subject
// ---------------------------------------------------------------------------

func TestClient_Introspect_Daemon(t *testing.T) {
	const payload = `{"ok":true,"data":{"pid":1234,"version":"0.5.0","uptime_secs":300,"socket_path":"/tmp/sock","config_path":"/home/user/.config/beachcomber/config.toml","requests_total":50,"in_flight":2,"active_watchers":3,"cache_entries":10,"verdicts":[{"level":"ok","message":"all good"}]}}`

	h, reqs := fixedHandler(payload)
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	resp, err := c.Introspect(beachcomber.SubjectDaemon, 0)
	if err != nil {
		t.Fatalf("Introspect: %v", err)
	}

	if resp.Subject != beachcomber.SubjectDaemon {
		t.Errorf("Subject = %v, want daemon", resp.Subject)
	}
	if resp.Daemon == nil {
		t.Fatal("Daemon is nil")
	}
	d := resp.Daemon
	if d.PID != 1234 {
		t.Errorf("PID = %d, want 1234", d.PID)
	}
	if d.Version != "0.5.0" {
		t.Errorf("Version = %q, want 0.5.0", d.Version)
	}
	if d.UptimeSecs != 300 {
		t.Errorf("UptimeSecs = %d, want 300", d.UptimeSecs)
	}
	if d.SocketPath != "/tmp/sock" {
		t.Errorf("SocketPath = %q, want /tmp/sock", d.SocketPath)
	}
	if d.ConfigPath != "/home/user/.config/beachcomber/config.toml" {
		t.Errorf("ConfigPath = %q", d.ConfigPath)
	}
	if d.RequestsTotal != 50 {
		t.Errorf("RequestsTotal = %d, want 50", d.RequestsTotal)
	}
	if d.InFlight != 2 {
		t.Errorf("InFlight = %d, want 2", d.InFlight)
	}
	if d.ActiveWatchers != 3 {
		t.Errorf("ActiveWatchers = %d, want 3", d.ActiveWatchers)
	}
	if d.CacheEntries != 10 {
		t.Errorf("CacheEntries = %d, want 10", d.CacheEntries)
	}
	if len(d.Verdicts) != 1 {
		t.Fatalf("len(Verdicts) = %d, want 1", len(d.Verdicts))
	}
	if d.Verdicts[0].Level != "ok" {
		t.Errorf("Verdicts[0].Level = %q, want ok", d.Verdicts[0].Level)
	}
	if d.Verdicts[0].Message != "all good" {
		t.Errorf("Verdicts[0].Message = %q, want all good", d.Verdicts[0].Message)
	}

	req := reqs()[0]
	if req["op"] != "introspect" {
		t.Errorf("op = %v, want introspect", req["op"])
	}
	if req["subject"] != "daemon" {
		t.Errorf("subject = %v, want daemon", req["subject"])
	}
	if _, hasDur := req["duration_secs"]; hasDur {
		t.Error("duration_secs should be absent when 0")
	}
}

func TestClient_Introspect_NonDaemon(t *testing.T) {
	h, reqs := fixedHandler(`{"ok":true,"data":{"providers":[]}}`)
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	resp, err := c.Introspect(beachcomber.SubjectProviders, 0)
	if err != nil {
		t.Fatalf("Introspect: %v", err)
	}

	if resp.Daemon != nil {
		t.Error("Daemon should be nil for non-daemon subject")
	}
	if resp.Other == nil {
		t.Error("Other should be populated for non-daemon subject")
	}
	if req := reqs()[0]; req["subject"] != "providers" {
		t.Errorf("subject = %v, want providers", req["subject"])
	}
}

func TestClient_Introspect_DurationSecs(t *testing.T) {
	h, reqs := fixedHandler(`{"ok":true,"data":[]}`)
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	_, err := c.Introspect(beachcomber.SubjectProcs, 5)
	if err != nil {
		t.Fatalf("Introspect: %v", err)
	}

	req := reqs()[0]
	// JSON numbers decode to float64
	if req["duration_secs"] != float64(5) {
		t.Errorf("duration_secs = %v, want 5", req["duration_secs"])
	}
}

// ---------------------------------------------------------------------------
// Client.Status (typed rows)
// ---------------------------------------------------------------------------

func TestClient_Status_Typed(t *testing.T) {
	const payload = `{"ok":true,"data":[{"provider":"git","field":"branch","path":"/repo","value":"main","age_ms":100,"stale":false},{"provider":"hostname","field":"short","value":"myhost","age_ms":200,"stale":true}]}`
	h, _ := fixedHandler(payload)
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	rows, err := c.Status()
	if err != nil {
		t.Fatalf("Status: %v", err)
	}

	if len(rows) != 2 {
		t.Fatalf("len(rows) = %d, want 2", len(rows))
	}

	r0 := rows[0]
	if r0.Provider != "git" {
		t.Errorf("rows[0].Provider = %q, want git", r0.Provider)
	}
	if r0.Field != "branch" {
		t.Errorf("rows[0].Field = %q, want branch", r0.Field)
	}
	if r0.Path != "/repo" {
		t.Errorf("rows[0].Path = %q, want /repo", r0.Path)
	}
	if r0.AgeMs != 100 {
		t.Errorf("rows[0].AgeMs = %d, want 100", r0.AgeMs)
	}
	if r0.Stale {
		t.Error("rows[0].Stale should be false")
	}

	r1 := rows[1]
	if r1.Provider != "hostname" {
		t.Errorf("rows[1].Provider = %q, want hostname", r1.Provider)
	}
	if !r1.Stale {
		t.Error("rows[1].Stale should be true")
	}
}

func TestClient_Status_NotArray(t *testing.T) {
	h, _ := fixedHandler(`{"ok":true,"data":{"unexpected":"object"}}`)
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	_, err := c.Status()
	if err == nil {
		t.Fatal("expected error when data is not array, got nil")
	}
}

func TestStatusRowExposesLifecycleFields(t *testing.T) {
	const payload = `{"ok":true,"data":[{"provider":"git","field":"branch","path":"/repo","value":"main","age_ms":100,"stale":false,"kind":{"kind":"lifecycle","decay":0,"watches_files":true},"poll_interval_secs":30,"keep_alive_polls":3,"fsevents_reinstate":false,"failure":{"consecutive_failures":1}}]}`
	h, _ := fixedHandler(payload)
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	rows, err := c.Status()
	if err != nil {
		t.Fatalf("Status: %v", err)
	}
	if len(rows) != 1 {
		t.Fatalf("len(rows) = %d, want 1", len(rows))
	}

	r := rows[0]
	if r.Provider != "git" {
		t.Errorf("Provider = %q, want git", r.Provider)
	}

	if r.Kind == nil {
		t.Fatal("Kind is nil, want non-nil map")
	}
	if r.Kind["kind"] != "lifecycle" {
		t.Errorf("Kind[\"kind\"] = %v, want lifecycle", r.Kind["kind"])
	}

	if r.PollIntervalSecs == nil {
		t.Fatal("PollIntervalSecs is nil, want non-nil")
	}
	if *r.PollIntervalSecs != 30 {
		t.Errorf("PollIntervalSecs = %d, want 30", *r.PollIntervalSecs)
	}

	if r.KeepAlivePolls == nil {
		t.Fatal("KeepAlivePolls is nil, want non-nil")
	}
	if *r.KeepAlivePolls != 3 {
		t.Errorf("KeepAlivePolls = %d, want 3", *r.KeepAlivePolls)
	}

	if r.FseventsReinstate == nil {
		t.Fatal("FseventsReinstate is nil, want non-nil")
	}
	if *r.FseventsReinstate != false {
		t.Errorf("FseventsReinstate = %v, want false", *r.FseventsReinstate)
	}

	if r.Failure == nil {
		t.Fatal("Failure is nil, want non-nil map")
	}
	if r.Failure["consecutive_failures"] != float64(1) {
		t.Errorf("Failure[\"consecutive_failures\"] = %v, want 1", r.Failure["consecutive_failures"])
	}
}

// ---------------------------------------------------------------------------
// Client.Watch
// ---------------------------------------------------------------------------

func TestClient_Watch_NextEvent(t *testing.T) {
	// The mock server sends two watch events then closes the connection.
	events := []string{
		`{"ok":true,"data":"main","age_ms":10,"stale":false}`,
		`{"ok":true,"data":"feature","age_ms":20,"stale":true}`,
	}
	evIdx := 0
	h := func(req map[string]interface{}) string {
		// The watch handler sends the first event as the response to the
		// watch request, then the mock closes. For streaming we rely on
		// the mock's per-message loop to send subsequent events.
		e := events[evIdx%len(events)]
		evIdx++
		return e
	}
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	ws, err := c.Watch("git.branch", "/repo")
	if err != nil {
		t.Fatalf("Watch: %v", err)
	}
	defer ws.Close()

	ev, err := ws.NextEvent()
	if err != nil {
		t.Fatalf("NextEvent: %v", err)
	}
	if ev == nil {
		t.Fatal("expected event, got nil")
	}
	if ev.Data != "main" {
		t.Errorf("Data = %v, want main", ev.Data)
	}
	if ev.AgeMs != 10 {
		t.Errorf("AgeMs = %d, want 10", ev.AgeMs)
	}
	if ev.Stale {
		t.Error("Stale should be false")
	}
}

func TestClient_Watch_ServerError(t *testing.T) {
	h, _ := fixedHandler(`{"ok":false,"error":"unknown key"}`)
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	ws, err := c.Watch("bad.key", "")
	if err != nil {
		t.Fatalf("Watch dial: %v", err)
	}
	defer ws.Close()

	_, err = ws.NextEvent()
	if err == nil {
		t.Fatal("expected error from server error event, got nil")
	}
}

// ---------------------------------------------------------------------------
// Session mirrors: GetWithFlags, Hello, Put, Introspect, Status
// ---------------------------------------------------------------------------

func TestSession_GetWithFlags(t *testing.T) {
	h, reqs := fixedHandler(`{"ok":true,"data":"v"}`)
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	sess, err := c.Session()
	if err != nil {
		t.Fatalf("Session: %v", err)
	}
	defer sess.Close()

	_, err = sess.GetWithFlags("x.y", "/p", true, false)
	if err != nil {
		t.Fatalf("GetWithFlags: %v", err)
	}

	req := reqs()[0]
	if req["force"] != true {
		t.Errorf("force = %v, want true", req["force"])
	}
	if _, hasWait := req["wait"]; hasWait {
		t.Error("wait should be absent when false")
	}
}

func TestSession_Hello(t *testing.T) {
	h, _ := fixedHandler(`{"ok":true,"data":{"protocol_version":"2","daemon_version":"1.0.0"}}`)
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	sess, err := c.Session()
	if err != nil {
		t.Fatalf("Session: %v", err)
	}
	defer sess.Close()

	info, err := sess.Hello()
	if err != nil {
		t.Fatalf("Hello: %v", err)
	}
	if info.ProtocolVersion != "2" {
		t.Errorf("ProtocolVersion = %q, want 2", info.ProtocolVersion)
	}
	if info.DaemonVersion != "1.0.0" {
		t.Errorf("DaemonVersion = %q, want 1.0.0", info.DaemonVersion)
	}
}

func TestSession_Put(t *testing.T) {
	h, reqs := fixedHandler(`{"ok":true}`)
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	sess, err := c.Session()
	if err != nil {
		t.Fatalf("Session: %v", err)
	}
	defer sess.Close()

	err = sess.Put("env.custom", map[string]interface{}{"color": "red"}, "", "")
	if err != nil {
		t.Fatalf("Put: %v", err)
	}
	if reqs()[0]["op"] != "put" {
		t.Errorf("op = %v, want put", reqs()[0]["op"])
	}
}

func TestSession_Introspect(t *testing.T) {
	h, reqs := fixedHandler(`{"ok":true,"data":{"providers":[]}}`)
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	sess, err := c.Session()
	if err != nil {
		t.Fatalf("Session: %v", err)
	}
	defer sess.Close()

	resp, err := sess.Introspect(beachcomber.SubjectConfig, 0)
	if err != nil {
		t.Fatalf("Introspect: %v", err)
	}
	if resp.Subject != beachcomber.SubjectConfig {
		t.Errorf("Subject = %v, want config", resp.Subject)
	}
	if reqs()[0]["subject"] != "config" {
		t.Errorf("subject = %v, want config", reqs()[0]["subject"])
	}
}

func TestSession_Status(t *testing.T) {
	h, _ := fixedHandler(`{"ok":true,"data":[{"provider":"battery","field":"pct","value":80,"age_ms":50}]}`)
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	sess, err := c.Session()
	if err != nil {
		t.Fatalf("Session: %v", err)
	}
	defer sess.Close()

	rows, err := sess.Status()
	if err != nil {
		t.Fatalf("Status: %v", err)
	}
	if len(rows) != 1 {
		t.Fatalf("len(rows) = %d, want 1", len(rows))
	}
	if rows[0].Provider != "battery" {
		t.Errorf("Provider = %q, want battery", rows[0].Provider)
	}
	if rows[0].AgeMs != 50 {
		t.Errorf("AgeMs = %d, want 50", rows[0].AgeMs)
	}
}
