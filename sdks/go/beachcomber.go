// Package beachcomber provides a client for the beachcomber daemon.
//
// The daemon caches shell-environment data (git state, hostname, battery, …)
// and serves it over a Unix domain socket using newline-delimited JSON. This
// SDK does not speak that protocol itself — it binds to libbeachcomber's C
// ABI (via purego; see ffi.go) and the shared library speaks it on the
// SDK's behalf. Discovery, socket retries, and the wire protocol all live in
// one place: libbeachcomber.
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
	"encoding/json"
	"fmt"
	"runtime"

	"github.com/ebitengine/purego"
)

// ---------------------------------------------------------------------------
// Types
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
	PollsElapsed      *uint64
	Failure           map[string]interface{} // e.g. {"consecutive_failures":3}
	Source            string                 // owning source name within the provider; empty when not set
}

// Verdict is one daemon-health assertion.
type Verdict struct {
	Level   string
	Message string
}

// ReaperInfo is the daemon's reaper capability snapshot, embedded in
// DaemonHealth when the daemon attaches one (canon singleton.md invariant
// 13). Absent (nil) for embedded/test servers.
type ReaperInfo struct {
	Armed      bool
	Visibility string // "system-wide" or "confined"
	Sweeps     uint64
	Reaped     uint64
	KillDenied uint64
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
	WatchBackend   string // "native", "polling", "disabled", "unknown", or "" if absent
	Reaper         *ReaperInfo
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

// WatchStream holds a handle to a watch subscription opened via bc_watch_open.
// Call Close to cancel and free it.
type WatchStream struct {
	lib    *nativeLib
	handle uintptr
}

// NextEvent blocks until the daemon emits the next change. Returns nil, nil
// when the stream ends (daemon closed the connection, or the watch was
// cancelled).
func (w *WatchStream) NextEvent() (*WatchEvent, error) {
	env, err := w.lib.callWatch(w.lib.bcWatchNext, w.handle, i32arg(-1))
	if err != nil {
		return nil, err
	}
	if !env.OK {
		kind, msg := "", ""
		if env.Error != nil {
			kind, msg = env.Error.Kind, env.Error.Message
		}
		return nil, &ServerError{Kind: kind, Message: msg, LibVersion: w.lib.version}
	}
	switch env.Outcome {
	case "event":
		return watchEventFromData(env.Data)
	case "eof", "cancelled", "timeout":
		return nil, nil
	default:
		return nil, &ProtocolError{msg: "unknown watch outcome: " + env.Outcome}
	}
}

// Close cancels and frees the watch handle, ending the stream. A NextEvent
// call already in flight on another goroutine observes the cancellation
// within one poll tick (see bc_watch_cancel's documentation); Close itself
// must not race a NextEvent call still in flight on the same handle.
func (w *WatchStream) Close() error {
	if w.handle != 0 {
		purego.SyscallN(w.lib.bcWatchCancel, w.handle)
		purego.SyscallN(w.lib.bcWatchFree, w.handle)
		w.handle = 0
		runtime.SetFinalizer(w, nil)
	}
	return nil
}

func (w *WatchStream) finalize() {
	if w.handle != 0 {
		purego.SyscallN(w.lib.bcWatchCancel, w.handle)
		purego.SyscallN(w.lib.bcWatchFree, w.handle)
	}
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

// Client sends individual requests; each call to the underlying library
// opens a fresh daemon connection (libbeachcomber's own behaviour — this
// SDK does not hold a socket open on the Client's behalf). For
// latency-sensitive use cases prefer [Session].
type Client struct {
	lib     *nativeLib
	handle  uintptr // BcClient*
	loadErr error   // set when the library failed to load; deferred to the first op
}

// Option customises client construction.
type Option func(opts map[string]interface{})

// WithAutostart sets whether the client forks a daemon when none is serving
// the socket. When the option is omitted the shared library's default
// applies (autostart on). Autostart only fires when the socket path is
// auto-discovered ([NewClient]): the shared library never autostarts for an
// explicit socket path, so this option is a no-op under [NewClientWithPath].
func WithAutostart(enabled bool) Option {
	return func(opts map[string]interface{}) { opts["autostart"] = enabled }
}

// NewClient returns a Client that auto-discovers the daemon socket path (the
// resolution libbeachcomber itself performs — see docs/canon/singleton.md).
// Unlike [NewClientWithPath] this may fail immediately: locating and
// validating libbeachcomber happens here, and a broken install (missing
// library, missing symbol) is reported now rather than deferred.
func NewClient(options ...Option) (*Client, error) {
	l, err := getLib()
	if err != nil {
		return nil, err
	}
	return newClientWithLib(l, "", options...)
}

// NewClientWithPath returns a Client that uses an explicit socket path.
// Mirrors libbeachcomber's own BcClient construction contract: this never
// fails from the caller's point of view. A library discovery/validation
// failure is recorded and surfaced on the Client's first operation instead.
func NewClientWithPath(socketPath string, options ...Option) *Client {
	l, err := getLib()
	if err != nil {
		return &Client{loadErr: err}
	}
	c, _ := newClientWithLib(l, socketPath, options...)
	return c
}

func newClientWithLib(l *nativeLib, socketPath string, options ...Option) (*Client, error) {
	opts := map[string]interface{}{}
	if socketPath != "" {
		opts["socket_path"] = socketPath
	}
	for _, o := range options {
		o(opts)
	}
	optsJSON, _ := json.Marshal(opts)
	optsBuf := cBytes(string(optsJSON))
	r1, _, _ := purego.SyscallN(l.bcClientNew, ptrOf(optsBuf))
	runtime.KeepAlive(optsBuf)

	c := &Client{lib: l, handle: r1}
	runtime.SetFinalizer(c, func(c *Client) { c.finalize() })
	return c, nil
}

func (c *Client) finalize() {
	if c.lib != nil && c.handle != 0 {
		purego.SyscallN(c.lib.bcClientFree, c.handle)
	}
}

// Get reads a cached value from the daemon.
//
// key follows the "provider" or "provider.field" format (e.g. "git" or
// "git.branch"). path is the working directory context; pass "" to omit it.
func (c *Client) Get(key string, path string) (*Result, error) {
	return c.GetWithFlags(key, path, false, false)
}

// GetWithFlags is like Get but supports force and wait flags.
func (c *Client) GetWithFlags(key, path string, force, wait bool) (*Result, error) {
	if c.loadErr != nil {
		return nil, c.loadErr
	}
	keyBuf, pathBuf := cBytes(key), optBytes(path)
	data, err := c.lib.call(c.lib.bcGet, c.handle, ptrOf(keyBuf), ptrOf(pathBuf), uintptr(getFlags(force, wait)))
	runtime.KeepAlive(keyBuf)
	runtime.KeepAlive(pathBuf)
	if err != nil {
		return nil, err
	}
	return resultFromGetData(data)
}

func getFlags(force, wait bool) uint32 {
	var f uint32
	if force {
		f |= 1 << 0 // BC_GET_FORCE
	}
	if wait {
		f |= 1 << 1 // BC_GET_WAIT
	}
	return f
}

// Refresh forces the daemon to recompute the given provider/key.
func (c *Client) Refresh(key string, path string) error {
	if c.loadErr != nil {
		return c.loadErr
	}
	keyBuf, pathBuf := cBytes(key), optBytes(path)
	_, err := c.lib.call(c.lib.bcRefresh, c.handle, ptrOf(keyBuf), ptrOf(pathBuf))
	runtime.KeepAlive(keyBuf)
	runtime.KeepAlive(pathBuf)
	return err
}

// Status returns cache rows from the daemon.
func (c *Client) Status() ([]CacheRow, error) {
	if c.loadErr != nil {
		return nil, c.loadErr
	}
	data, err := c.lib.call(c.lib.bcStatus, c.handle)
	if err != nil {
		return nil, err
	}
	return parseCacheRowsFromData(data)
}

// Session opens a persistent connection and returns a [Session].
func (c *Client) Session() (*Session, error) {
	if c.loadErr != nil {
		return nil, c.loadErr
	}
	r1, _, _ := purego.SyscallN(c.lib.bcSessionOpen, c.handle)
	s := &Session{client: c, lib: c.lib, handle: r1}
	runtime.SetFinalizer(s, func(s *Session) { s.finalize() })
	return s, nil
}

// Hello returns the daemon's protocol and build versions.
func (c *Client) Hello() (*HelloInfo, error) {
	if c.loadErr != nil {
		return nil, c.loadErr
	}
	data, err := c.lib.call(c.lib.bcHello, c.handle)
	if err != nil {
		return nil, err
	}
	return parseHelloFromData(data)
}

// Put stores data into a virtual provider. data should be a JSON object.
// ttl and path are optional; pass "" to omit them.
func (c *Client) Put(key string, data interface{}, ttl, path string) error {
	if c.loadErr != nil {
		return c.loadErr
	}
	dataJSON, err := json.Marshal(data)
	if err != nil {
		return fmt.Errorf("beachcomber: marshal put data: %w", err)
	}
	keyBuf, dataBuf, ttlBuf, pathBuf := cBytes(key), cBytes(string(dataJSON)), optBytes(ttl), optBytes(path)
	_, err = c.lib.call(c.lib.bcPut, c.handle, ptrOf(keyBuf), ptrOf(dataBuf), ptrOf(ttlBuf), ptrOf(pathBuf))
	runtime.KeepAlive(keyBuf)
	runtime.KeepAlive(dataBuf)
	runtime.KeepAlive(ttlBuf)
	runtime.KeepAlive(pathBuf)
	return err
}

// Introspect runs a diagnostic query. durationSecs is only consulted for
// SubjectProcs; pass 0 otherwise.
func (c *Client) Introspect(subject IntrospectSubject, durationSecs uint64) (*IntrospectResponse, error) {
	if c.loadErr != nil {
		return nil, c.loadErr
	}
	subjectBuf := cBytes(string(subject))
	var optsBuf []byte
	if durationSecs > 0 {
		optsJSON, _ := json.Marshal(map[string]uint64{"duration_secs": durationSecs})
		optsBuf = cBytes(string(optsJSON))
	}
	data, err := c.lib.call(c.lib.bcIntrospect, c.handle, ptrOf(subjectBuf), ptrOf(optsBuf))
	runtime.KeepAlive(subjectBuf)
	runtime.KeepAlive(optsBuf)
	if err != nil {
		return nil, err
	}
	return parseIntrospectFromData(subject, data)
}

// Watch subscribes to changes on a key. The returned stream holds a watch
// handle; call Close on it to cancel and free it.
func (c *Client) Watch(key, path string) (*WatchStream, error) {
	if c.loadErr != nil {
		return nil, c.loadErr
	}
	keyBuf, pathBuf := cBytes(key), optBytes(path)
	r1, _, _ := purego.SyscallN(c.lib.bcWatchOpen, c.handle, ptrOf(keyBuf), ptrOf(pathBuf))
	runtime.KeepAlive(keyBuf)
	runtime.KeepAlive(pathBuf)
	if r1 == 0 {
		return nil, &LibraryError{Message: "beachcomber: bc_watch_open: allocation failure", LibVersion: c.lib.version}
	}
	w := &WatchStream{lib: c.lib, handle: r1}
	runtime.SetFinalizer(w, func(w *WatchStream) { w.finalize() })
	return w, nil
}
