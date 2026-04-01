package beachcomber_test

import (
	"bufio"
	"encoding/json"
	"errors"
	"net"
	"os"
	"path/filepath"
	"testing"

	beachcomber "github.com/NavistAu/beachcomber/sdks/go"
)

// ---------------------------------------------------------------------------
// Mock server helpers
// ---------------------------------------------------------------------------

// handler is called once per request JSON object. It returns the raw JSON line
// to write back (without a trailing newline — the server adds it).
type handler func(req map[string]interface{}) string

// startMockServer starts a Unix socket server that handles one connection at a
// time. Each incoming newline-delimited JSON request is passed to h; the
// return value is sent back as a newline-delimited JSON response.
//
// The listener is closed when t.Cleanup runs.
func startMockServer(t *testing.T, h handler) string {
	t.Helper()
	dir := t.TempDir()
	sockPath := filepath.Join(dir, "sock")

	ln, err := net.Listen("unix", sockPath)
	if err != nil {
		t.Fatalf("listen: %v", err)
	}

	t.Cleanup(func() { ln.Close() })

	go func() {
		for {
			conn, err := ln.Accept()
			if err != nil {
				return // listener closed
			}
			go func(c net.Conn) {
				defer c.Close()
				scanner := bufio.NewScanner(c)
				for scanner.Scan() {
					var req map[string]interface{}
					if err := json.Unmarshal(scanner.Bytes(), &req); err != nil {
						c.Write([]byte(`{"ok":false,"error":"bad request"}` + "\n"))
						continue
					}
					resp := h(req)
					c.Write([]byte(resp + "\n"))
				}
			}(conn)
		}
	}()

	return sockPath
}

// echoHandler returns a simple handler that stores received requests so tests
// can inspect them, and responds with a fixed JSON string.
func fixedHandler(response string) (handler, func() []map[string]interface{}) {
	var requests []map[string]interface{}
	h := func(req map[string]interface{}) string {
		requests = append(requests, req)
		return response
	}
	return h, func() []map[string]interface{} { return requests }
}

// ---------------------------------------------------------------------------
// Client.Get
// ---------------------------------------------------------------------------

func TestClient_Get_Hit(t *testing.T) {
	h, reqs := fixedHandler(`{"ok":true,"data":"main","age_ms":123,"stale":false}`)
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	result, err := c.Get("git.branch", "/repo")
	if err != nil {
		t.Fatalf("Get: %v", err)
	}

	if !result.IsHit() {
		t.Error("expected IsHit() = true")
	}
	s, ok := result.GetString("")
	if !ok || s != "main" {
		t.Errorf("GetString = (%q, %v), want (\"main\", true)", s, ok)
	}
	if result.AgeMs != 123 {
		t.Errorf("AgeMs = %d, want 123", result.AgeMs)
	}

	got := reqs()[0]
	if got["op"] != "get" {
		t.Errorf("op = %v, want get", got["op"])
	}
	if got["key"] != "git.branch" {
		t.Errorf("key = %v, want git.branch", got["key"])
	}
	if got["path"] != "/repo" {
		t.Errorf("path = %v, want /repo", got["path"])
	}
}

func TestClient_Get_Miss(t *testing.T) {
	h, _ := fixedHandler(`{"ok":true}`)
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	result, err := c.Get("git.branch", "")
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	if !result.IsMiss() {
		t.Error("expected IsMiss() = true")
	}
}

func TestClient_Get_NoPath(t *testing.T) {
	h, reqs := fixedHandler(`{"ok":true,"data":"val"}`)
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	_, err := c.Get("hostname.short", "")
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	req := reqs()[0]
	if _, hasPath := req["path"]; hasPath {
		t.Error("path field should be absent when path is empty string")
	}
}

func TestClient_Get_ServerError(t *testing.T) {
	h, _ := fixedHandler(`{"ok":false,"error":"unknown provider: foo"}`)
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	_, err := c.Get("foo.bar", "")
	if err == nil {
		t.Fatal("expected error, got nil")
	}
	var se *beachcomber.ServerError
	if !errors.As(err, &se) {
		t.Errorf("expected *ServerError, got %T: %v", err, err)
	}
	if se.Message != "unknown provider: foo" {
		t.Errorf("ServerError.Message = %q", se.Message)
	}
}

// ---------------------------------------------------------------------------
// Client.Poke
// ---------------------------------------------------------------------------

func TestClient_Poke(t *testing.T) {
	h, reqs := fixedHandler(`{"ok":true}`)
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	if err := c.Poke("git", "/repo"); err != nil {
		t.Fatalf("Poke: %v", err)
	}
	req := reqs()[0]
	if req["op"] != "poke" {
		t.Errorf("op = %v, want poke", req["op"])
	}
	if req["path"] != "/repo" {
		t.Errorf("path = %v, want /repo", req["path"])
	}
}

// ---------------------------------------------------------------------------
// Client.List
// ---------------------------------------------------------------------------

func TestClient_List(t *testing.T) {
	listData := `[{"name":"git","global":false,"fields":["branch","dirty"]}]`
	h, reqs := fixedHandler(`{"ok":true,"data":` + listData + `}`)
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	result, err := c.List()
	if err != nil {
		t.Fatalf("List: %v", err)
	}
	if !result.IsHit() {
		t.Error("expected IsHit() = true")
	}
	if reqs()[0]["op"] != "list" {
		t.Errorf("op = %v, want list", reqs()[0]["op"])
	}
}

// ---------------------------------------------------------------------------
// Client.Status
// ---------------------------------------------------------------------------

func TestClient_Status(t *testing.T) {
	h, reqs := fixedHandler(`{"ok":true,"data":{"providers":1}}`)
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	result, err := c.Status()
	if err != nil {
		t.Fatalf("Status: %v", err)
	}
	if !result.IsHit() {
		t.Error("expected IsHit() = true")
	}
	if reqs()[0]["op"] != "status" {
		t.Errorf("op = %v, want status", reqs()[0]["op"])
	}
}

// ---------------------------------------------------------------------------
// Connection refused / daemon not running
// ---------------------------------------------------------------------------

func TestClient_Get_DaemonNotRunning(t *testing.T) {
	// Use a path that does not have a socket.
	sock := filepath.Join(t.TempDir(), "nonexistent.sock")
	c := beachcomber.NewClientWithPath(sock)
	_, err := c.Get("git.branch", "")
	if err == nil {
		t.Fatal("expected error, got nil")
	}
	if !errors.Is(err, beachcomber.ErrDaemonNotRunning) {
		t.Errorf("expected ErrDaemonNotRunning, got %T: %v", err, err)
	}
}

// ---------------------------------------------------------------------------
// Malformed response
// ---------------------------------------------------------------------------

func TestClient_Get_MalformedResponse(t *testing.T) {
	h, _ := fixedHandler(`not valid json`)
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	_, err := c.Get("git.branch", "")
	if err == nil {
		t.Fatal("expected error on malformed JSON, got nil")
	}
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

func TestSession_MultipleGets(t *testing.T) {
	responses := []string{
		`{"ok":true,"data":"main"}`,
		`{"ok":true,"data":true}`,
	}
	idx := 0
	h := func(req map[string]interface{}) string {
		r := responses[idx]
		idx++
		return r
	}
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	sess, err := c.Session()
	if err != nil {
		t.Fatalf("Session: %v", err)
	}
	defer sess.Close()

	r1, err := sess.Get("git.branch", "/repo")
	if err != nil {
		t.Fatalf("first Get: %v", err)
	}
	if s, _ := r1.GetString(""); s != "main" {
		t.Errorf("first result = %q, want main", s)
	}

	r2, err := sess.Get("git.dirty", "")
	if err != nil {
		t.Fatalf("second Get: %v", err)
	}
	if b, _ := r2.GetBool(""); !b {
		t.Error("second result should be true")
	}
}

func TestSession_SetContext(t *testing.T) {
	var lastReq map[string]interface{}
	h := func(req map[string]interface{}) string {
		lastReq = req
		return `{"ok":true}`
	}
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	sess, err := c.Session()
	if err != nil {
		t.Fatalf("Session: %v", err)
	}
	defer sess.Close()

	if err := sess.SetContext("/my/repo"); err != nil {
		t.Fatalf("SetContext: %v", err)
	}
	if lastReq["op"] != "context" {
		t.Errorf("op = %v, want context", lastReq["op"])
	}
	if lastReq["path"] != "/my/repo" {
		t.Errorf("path = %v, want /my/repo", lastReq["path"])
	}
}

func TestSession_Poke(t *testing.T) {
	var lastReq map[string]interface{}
	h := func(req map[string]interface{}) string {
		lastReq = req
		return `{"ok":true}`
	}
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	sess, err := c.Session()
	if err != nil {
		t.Fatalf("Session: %v", err)
	}
	defer sess.Close()

	if err := sess.Poke("git", ""); err != nil {
		t.Fatalf("Poke: %v", err)
	}
	if lastReq["op"] != "poke" {
		t.Errorf("op = %v, want poke", lastReq["op"])
	}
	if _, hasPath := lastReq["path"]; hasPath {
		t.Error("path field should be absent when path is empty string")
	}
}

func TestSession_Close(t *testing.T) {
	h, _ := fixedHandler(`{"ok":true}`)
	sock := startMockServer(t, h)

	c := beachcomber.NewClientWithPath(sock)
	sess, err := c.Session()
	if err != nil {
		t.Fatalf("Session: %v", err)
	}
	if err := sess.Close(); err != nil {
		t.Errorf("Close: %v", err)
	}
}

// ---------------------------------------------------------------------------
// NewClient auto-discovery smoke test
// ---------------------------------------------------------------------------

func TestNewClient_AutoDiscovery(t *testing.T) {
	// Create a real socket so DiscoverSocketPath can find it via XDG.
	dir := t.TempDir()
	sockDir := filepath.Join(dir, "beachcomber")
	if err := os.MkdirAll(sockDir, 0o700); err != nil {
		t.Fatal(err)
	}
	sockPath := filepath.Join(sockDir, "sock")

	h, _ := fixedHandler(`{"ok":true,"data":"main"}`)
	ln, err := net.Listen("unix", sockPath)
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	t.Cleanup(func() { ln.Close() })
	go func() {
		for {
			conn, err := ln.Accept()
			if err != nil {
				return
			}
			go func(c net.Conn) {
				defer c.Close()
				scanner := bufio.NewScanner(c)
				for scanner.Scan() {
					var req map[string]interface{}
					json.Unmarshal(scanner.Bytes(), &req)
					c.Write([]byte(h(req) + "\n"))
				}
			}(conn)
		}
	}()

	t.Setenv("XDG_RUNTIME_DIR", dir)

	c, err := beachcomber.NewClient()
	if err != nil {
		t.Fatalf("NewClient: %v", err)
	}
	result, err := c.Get("git.branch", "")
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	if !result.IsHit() {
		t.Error("expected IsHit() = true")
	}
}

// ---------------------------------------------------------------------------
// Table-driven: Result accessor edge cases via mock server
// ---------------------------------------------------------------------------

func TestClient_Get_TableDriven(t *testing.T) {
	cases := []struct {
		name     string
		response string
		wantHit  bool
		wantMiss bool
		wantErr  bool
	}{
		{
			name:     "hit with string data",
			response: `{"ok":true,"data":"v1","age_ms":1}`,
			wantHit:  true,
		},
		{
			name:     "miss no data field",
			response: `{"ok":true}`,
			wantMiss: true,
		},
		{
			name:     "miss explicit null",
			response: `{"ok":true,"data":null}`,
			wantMiss: true,
		},
		{
			name:     "server error",
			response: `{"ok":false,"error":"oops"}`,
			wantErr:  true,
		},
		{
			name:     "malformed json",
			response: `{bad json`,
			wantErr:  true,
		},
	}

	for _, tc := range cases {
		tc := tc
		t.Run(tc.name, func(t *testing.T) {
			h, _ := fixedHandler(tc.response)
			sock := startMockServer(t, h)

			c := beachcomber.NewClientWithPath(sock)
			result, err := c.Get("x", "")

			if tc.wantErr {
				if err == nil {
					t.Errorf("expected error, got nil (result=%+v)", result)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if tc.wantHit && !result.IsHit() {
				t.Error("expected IsHit() = true")
			}
			if tc.wantMiss && !result.IsMiss() {
				t.Error("expected IsMiss() = true")
			}
		})
	}
}
