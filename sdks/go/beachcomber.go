// Package beachcomber provides a client for the beachcomber daemon.
//
// The daemon caches shell-environment data (git state, hostname, battery, …)
// and serves it over a Unix domain socket using newline-delimited JSON.
//
// Quick start — one-shot queries:
//
//	c, err := beachcomber.NewClient()
//	if err != nil {
//	    log.Fatal(err)
//	}
//	result, err := c.Get("git.branch", "/path/to/repo")
//
// For prompts and tools that make many queries per invocation, open a
// persistent [Session] instead:
//
//	sess, err := c.Session()
//	if err != nil { … }
//	defer sess.Close()
//	sess.SetContext("/path/to/repo")
//	result, err := sess.Get("git.branch", "")
package beachcomber

import (
	"bufio"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"strings"
	"time"
)

// ---------------------------------------------------------------------------
// New types
// ---------------------------------------------------------------------------

// HelloInfo is returned by Hello.
type HelloInfo struct {
	ProtocolVersion string
	DaemonVersion   string
}

// CacheRow is one row of Status response.
type CacheRow struct {
	Provider          string
	Field             string // empty when not set
	Path              string // empty when not set
	Value             interface{}
	AgeMs             uint64
	Stale             bool
	Kind              map[string]interface{} // e.g. {"kind":"lifecycle","decay":0,"watches_files":true}
	PollIntervalSecs  *uint64
	KeepAlivePolls    *uint32
	FseventsReinstate *bool
	Failure           map[string]interface{} // e.g. {"consecutive_failures":3}
}

// Verdict is one daemon-health assertion.
type Verdict struct {
	Level   string
	Message string
}

// DaemonHealth is the typed response for Introspect(SubjectDaemon).
type DaemonHealth struct {
	PID            int64
	Version        string
	UptimeSecs     uint64
	SocketPath     string
	ConfigPath     string // empty when null
	RequestsTotal  uint64
	InFlight       uint64
	ActiveWatchers uint64
	CacheEntries   uint64
	Verdicts       []Verdict
}

// IntrospectSubject names an introspect target.
type IntrospectSubject string

const (
	SubjectDaemon    IntrospectSubject = "daemon"
	SubjectProviders IntrospectSubject = "providers"
	SubjectConfig    IntrospectSubject = "config"
	SubjectCache     IntrospectSubject = "cache"
	SubjectLifecycle IntrospectSubject = "lifecycle"
	SubjectWatches   IntrospectSubject = "watches"
	SubjectTimers    IntrospectSubject = "timers"
	SubjectDemand    IntrospectSubject = "demand"
	SubjectProcs     IntrospectSubject = "procs"
)

// IntrospectResponse wraps an Introspect reply. When Subject==Daemon,
// Daemon is populated; otherwise Other holds the raw JSON value.
type IntrospectResponse struct {
	Subject IntrospectSubject
	Daemon  *DaemonHealth
	Other   interface{}
}

// WatchEvent is one event from a watch stream.
type WatchEvent struct {
	Data  interface{}
	AgeMs uint64
	Stale bool
}

// WatchStream holds a dedicated connection for watching key changes.
// Drop it (call Close) to disconnect.
type WatchStream struct {
	conn    net.Conn
	scanner *bufio.Scanner
}

// NextEvent blocks until the daemon emits the next change. Returns nil, nil on
// connection close.
func (w *WatchStream) NextEvent() (*WatchEvent, error) {
	if !w.scanner.Scan() {
		if err := w.scanner.Err(); err != nil {
			return nil, err
		}
		return nil, nil
	}
	var resp response
	if err := json.Unmarshal(w.scanner.Bytes(), &resp); err != nil {
		return nil, &ProtocolError{msg: "watch event parse: " + err.Error()}
	}
	if !resp.OK {
		return nil, &ServerError{Message: resp.Error}
	}
	var data interface{}
	if len(resp.Data) > 0 && string(resp.Data) != "null" {
		json.Unmarshal(resp.Data, &data) //nolint:errcheck
	}
	return &WatchEvent{Data: data, AgeMs: resp.AgeMs, Stale: resp.Stale}, nil
}

// Close closes the underlying watch connection.
func (w *WatchStream) Close() error { return w.conn.Close() }

const defaultTimeout = 100 * time.Millisecond

// ErrDaemonNotRunning is returned when the Unix socket cannot be reached.
var ErrDaemonNotRunning = errors.New("beachcomber daemon is not running")

// ServerError is returned when the daemon responds with ok:false.
type ServerError struct {
	Message string
}

func (e *ServerError) Error() string {
	return fmt.Sprintf("beachcomber: daemon error: %s", e.Message)
}

// ProtocolError is returned when a response cannot be parsed.
type ProtocolError struct {
	msg string
}

func (e *ProtocolError) Error() string {
	return fmt.Sprintf("beachcomber: protocol error: %s", e.msg)
}

// Client sends individual requests, opening a fresh connection for each call.
// For latency-sensitive use cases prefer [Session].
type Client struct {
	socketPath string
	timeout    time.Duration
}

// NewClient returns a Client that auto-discovers the daemon socket path.
// Returns [ErrDaemonNotRunning] wrapped in an error if discovery fails (i.e.,
// none of the candidate paths resolve to a reachable socket). The error is
// deferred to the first actual network call; NewClient itself never dials.
func NewClient() (*Client, error) {
	path := DiscoverSocketPath()
	return &Client{
		socketPath: path,
		timeout:    defaultTimeout,
	}, nil
}

// NewClientWithPath returns a Client that uses an explicit socket path.
func NewClientWithPath(socketPath string) *Client {
	return &Client{
		socketPath: socketPath,
		timeout:    defaultTimeout,
	}
}

// Get reads a cached value from the daemon.
//
// key follows the "provider" or "provider.field" format (e.g. "git" or
// "git.branch"). path is the working directory context; pass "" to omit it.
func (c *Client) Get(key string, path string) (*Result, error) {
	req := map[string]interface{}{"op": "get", "key": key}
	if path != "" {
		req["path"] = path
	}
	return c.roundtrip(req)
}

// Refresh forces the daemon to recompute the given provider/key.
func (c *Client) Refresh(key string, path string) error {
	req := map[string]interface{}{"op": "refresh", "key": key}
	if path != "" {
		req["path"] = path
	}
	_, err := c.roundtrip(req)
	return err
}

// Status returns cache rows from the daemon.
func (c *Client) Status() ([]CacheRow, error) {
	result, err := c.roundtrip(map[string]interface{}{"op": "status"})
	if err != nil {
		return nil, err
	}
	return parseCacheRowsFromResult(result)
}

// Session opens a persistent connection and returns a [Session].
func (c *Client) Session() (*Session, error) {
	conn, err := c.dial()
	if err != nil {
		return nil, err
	}
	return &Session{
		conn:    conn,
		scanner: bufio.NewScanner(conn),
	}, nil
}

// GetWithFlags is like Get but supports force and wait flags.
func (c *Client) GetWithFlags(key, path string, force, wait bool) (*Result, error) {
	req := map[string]interface{}{"op": "get", "key": key}
	if path != "" {
		req["path"] = path
	}
	if force {
		req["force"] = true
	}
	if wait {
		req["wait"] = true
	}
	return c.roundtrip(req)
}

// Hello returns the daemon's protocol and build versions.
func (c *Client) Hello() (*HelloInfo, error) {
	result, err := c.roundtrip(map[string]interface{}{"op": "hello"})
	if err != nil {
		return nil, err
	}
	return parseHelloFromResult(result)
}

// Put stores data into a virtual provider. data should be a JSON object.
// ttl and path are optional; pass "" to omit them.
func (c *Client) Put(key string, data interface{}, ttl, path string) error {
	req := map[string]interface{}{"op": "put", "key": key, "data": data}
	if ttl != "" {
		req["ttl"] = ttl
	}
	if path != "" {
		req["path"] = path
	}
	_, err := c.roundtrip(req)
	return err
}

// Introspect runs a diagnostic query. durationSecs is only consulted for
// SubjectProcs; pass 0 otherwise.
func (c *Client) Introspect(subject IntrospectSubject, durationSecs uint64) (*IntrospectResponse, error) {
	req := map[string]interface{}{"op": "introspect", "subject": string(subject)}
	if durationSecs > 0 {
		req["duration_secs"] = durationSecs
	}
	result, err := c.roundtrip(req)
	if err != nil {
		return nil, err
	}
	return parseIntrospectFromResult(subject, result)
}

// Watch subscribes to changes on a key. The returned stream holds a dedicated
// connection; call Close on it to disconnect.
func (c *Client) Watch(key, path string) (*WatchStream, error) {
	conn, err := c.dial()
	if err != nil {
		return nil, err
	}
	req := map[string]interface{}{"op": "watch", "key": key}
	if path != "" {
		req["path"] = path
	}
	if err := writeJSON(conn, req); err != nil {
		conn.Close()
		return nil, err
	}
	return &WatchStream{conn: conn, scanner: bufio.NewScanner(conn)}, nil
}

// roundtrip dials, sends one request, reads one response, and closes.
func (c *Client) roundtrip(req map[string]interface{}) (*Result, error) {
	conn, err := c.dial()
	if err != nil {
		return nil, err
	}
	defer conn.Close()

	if err := writeJSON(conn, req); err != nil {
		return nil, err
	}

	scanner := bufio.NewScanner(conn)
	return readResponse(scanner)
}

var retryBackoffs = []time.Duration{
	250 * time.Millisecond,
	500 * time.Millisecond,
	1000 * time.Millisecond,
}

// connectWithRetry dials a Unix socket with 3 retries (250ms/500ms/1s
// exponential backoff).  Retries only on connection-refused and
// no-such-file errors — other errors surface immediately.
func connectWithRetry(path string, timeout time.Duration) (net.Conn, error) {
	var lastErr error
	for _, backoff := range retryBackoffs {
		conn, err := net.DialTimeout("unix", path, timeout)
		if err == nil {
			return conn, nil
		}
		if !isRetriable(err) {
			return nil, err
		}
		lastErr = err
		time.Sleep(backoff)
	}
	// Final attempt.
	conn, err := net.DialTimeout("unix", path, timeout)
	if err != nil {
		if lastErr != nil {
			return nil, lastErr
		}
		return nil, err
	}
	return conn, nil
}

// isRetriable returns true for transient connect errors that may resolve
// when the daemon finishes restarting (connection refused / socket absent).
func isRetriable(err error) bool {
	if err == nil {
		return false
	}
	s := err.Error()
	return strings.Contains(s, "connection refused") ||
		strings.Contains(s, "no such file or directory")
}

func (c *Client) dial() (net.Conn, error) {
	conn, err := connectWithRetry(c.socketPath, c.timeout)
	if err != nil {
		return nil, fmt.Errorf("%w: %s", ErrDaemonNotRunning, c.socketPath)
	}
	return conn, nil
}

// writeJSON serialises req as a single JSON line.
func writeJSON(conn net.Conn, req map[string]interface{}) error {
	data, err := json.Marshal(req)
	if err != nil {
		return fmt.Errorf("beachcomber: marshal: %w", err)
	}
	data = append(data, '\n')
	_, err = conn.Write(data)
	return err
}

// readResponse reads one line from scanner and parses it as a daemon response.
func readResponse(scanner *bufio.Scanner) (*Result, error) {
	if !scanner.Scan() {
		if err := scanner.Err(); err != nil {
			return nil, fmt.Errorf("beachcomber: read: %w", err)
		}
		return nil, &ProtocolError{msg: "connection closed before response"}
	}
	return parseResponse(scanner.Bytes())
}
