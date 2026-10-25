// Package beachcomber provides a client for the beachcomber daemon.
//
// The daemon caches shell-environment data (git state, hostname, battery, …)
// and serves it over a Unix domain socket using newline-delimited JSON. This
// SDK does not speak that protocol itself — it binds to libbeachcomber's C
// ABI (via purego; see ffi.go) and the shared library speaks it on the
// SDK's behalf. Discovery, socket retries, and the wire protocol all live in
// one place: libbeachcomber.
//
// The package follows the SDK API table every beachcomber SDK follows
// (docs/sdk-api.md): the same operations, arguments, value shapes and error
// kinds, in Go's casing.
//
// Quick start — one-shot queries:
//
//	c, err := beachcomber.NewClient()
//	if err != nil {
//	    log.Fatal(err)
//	}
//	defer c.Close()
//	branch, err := c.Get("git.branch", "/path/to/repo", nil) // what comb get prints there; nil on a miss
//	tmpl, err := c.Get("git:template.branch", "", nil)      // the template, unrendered
//
// For prompts and tools that make many queries per invocation, open a
// persistent [Session] instead:
//
//	sess, err := c.Session()
//	if err != nil { … }
//	defer sess.Close()
//	sess.SetContext("/path/to/repo")
//	branch, err := sess.Get("git.branch", "", nil)
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
	Kind              map[string]interface{} // e.g. {"kind":"lifecycle","decay":0,"depth":4,"watches_files":true}
	PollIntervalSecs  *uint64
	PollCount         *uint32
	FseventsReinstate *bool
	PollsElapsed      *uint64
	NextPollInSecs    *uint64                // seconds until the next scheduled poll; nil when it has none
	Failure           map[string]interface{} // e.g. {"consecutive_failures":3}
	Source            string                 // owning source name within the provider; empty when not set
}

// Verdict is one daemon-health assertion.
type Verdict struct {
	Level   string
	Message string
}

// ReaperStatus is the daemon's reaper capability snapshot, embedded in
// DaemonHealth when the daemon attaches one. Absent (nil) for
// embedded/test servers.
type ReaperStatus struct {
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
	Reaper         *ReaperStatus
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

// WatchEvent is one emitted change on a watch. Data is the value Get answers
// for the watched key at that moment, or nil when nothing answers there — a
// miss, not the end of the stream. A value resolved from several cache
// records has no single age or staleness, so a watch reports neither.
type WatchEvent struct {
	Data interface{}
}

// WatchStream holds a handle to a watch subscription opened via bc_watch_open.
// Each event is the value Get answers for the watched key, re-resolved
// whenever any key the resolution reads changes, including the provider's
// template tree. Call Close to cancel and free it.
type WatchStream struct {
	lib    *nativeLib
	handle uintptr
}

// NextEvent waits for the watched value to change and returns the event.
// With no argument it waits indefinitely; NextEvent(ms) waits at most ms
// milliseconds (0 polls) and, if no event arrives, returns an *Error of
// kind timeout (errors.Is(err, ErrTimeout)). It returns nil, nil once the
// stream has ended — the daemon closed it, or Cancel or Close was called.
func (w *WatchStream) NextEvent(timeoutMs ...int32) (*WatchEvent, error) {
	if w.handle == 0 {
		return nil, nil
	}
	wait := int32(-1)
	if len(timeoutMs) > 0 {
		wait = timeoutMs[0]
	}
	env, err := w.lib.callWatch(w.lib.bcWatchNext, w.handle, i32arg(wait))
	if err != nil {
		return nil, err
	}
	if !env.OK {
		return nil, w.lib.envelopeError(env.Error, "bc_watch_next")
	}
	switch env.Outcome {
	case "event":
		ev, err := watchEventFromData(env.Data)
		return ev, w.lib.stamp(err)
	case "eof", "cancelled":
		return nil, nil
	case "timeout":
		return nil, &Error{
			Kind:           "timeout",
			Message:        fmt.Sprintf("no event within %dms", wait),
			LibraryVersion: w.lib.version,
		}
	default:
		return nil, w.lib.stamp(parseError("unknown watch outcome: " + env.Outcome))
	}
}

// Cancel ends the stream: a NextEvent waiting on another goroutine returns
// nil, nil within one poll tick (see bc_watch_cancel's documentation), as
// does every later one. The handle stays allocated until Close.
func (w *WatchStream) Cancel() error {
	if w.handle != 0 {
		purego.SyscallN(w.lib.bcWatchCancel, w.handle)
	}
	return nil
}

// Close cancels and frees the watch handle, ending the stream. Close itself
// must not race a NextEvent call still in flight on the same handle; Cancel
// first, from the other goroutine, to end one.
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
// latency-sensitive use cases prefer [Session]. Call Close when done.
type Client struct {
	lib    *nativeLib
	handle uintptr // BcClient*; 0 once closed
}

// Option customises client construction: each writes one key of the
// options_json bc_client_new takes. An option left out is the library's
// default; this SDK has none of its own.
type Option func(opts map[string]interface{})

// WithSocketPath sets the daemon's socket (bc_client_new's socket_path).
// Left out, the library discovers it: $BEACHCOMBER_SOCKET, else
// /tmp/beachcomber-<uid>/sock.
func WithSocketPath(path string) Option {
	return func(opts map[string]interface{}) { opts["socket_path"] = path }
}

// WithAutostart sets whether the client forks a daemon when none is serving
// the socket. Left out, the library's default applies (autostart on).
// Autostart only fires when the socket path is auto-discovered: the shared
// library never autostarts for an explicit [WithSocketPath].
func WithAutostart(enabled bool) Option {
	return func(opts map[string]interface{}) { opts["autostart"] = enabled }
}

// WithTimeoutMs sets how long one read or write on the daemon's socket may
// take, in milliseconds (bc_client_new's timeout_ms). Left out, the
// library's default applies (100ms); this SDK has none of its own. 0 is
// refused: the client's first operation fails with kind parse_error.
func WithTimeoutMs(ms uint64) Option {
	return func(opts map[string]interface{}) { opts["timeout_ms"] = ms }
}

// WithConnectTimeoutMs sets how long connecting keeps retrying a socket
// nobody answers yet, in milliseconds (bc_client_new's connect_timeout_ms);
// 0 is a single attempt. Left out, the library's default applies (1500ms);
// this SDK has none of its own.
func WithConnectTimeoutMs(ms uint64) Option {
	return func(opts map[string]interface{}) { opts["connect_timeout_ms"] = ms }
}

// WithExec sets whether a template this client renders ([Client.Get],
// [Client.Eval]) may call exec(). Off by default: a template that calls it
// then fails with "exec() is disabled in this context".
func WithExec(enabled bool) Option {
	return func(opts map[string]interface{}) { opts["exec"] = enabled }
}

// NewClient returns a Client made with options. Locating and validating
// libbeachcomber happens here, so a broken install (kind library_not_found
// or library_symbol) is reported now. A malformed option is the library's
// to refuse, on the client's first operation (kind parse_error).
func NewClient(options ...Option) (*Client, error) {
	l, err := getLib()
	if err != nil {
		return nil, err
	}
	optsBuf := cBytes(optionsJSON(options...))
	r1, _, _ := purego.SyscallN(l.bcClientNew, ptrOf(optsBuf))
	runtime.KeepAlive(optsBuf)

	c := &Client{lib: l, handle: r1}
	runtime.SetFinalizer(c, func(c *Client) { c.finalize() })
	return c, nil
}

// optionsJSON builds the options_json bc_client_new takes: the keys the
// options write, and no others.
func optionsJSON(options ...Option) string {
	opts := map[string]interface{}{}
	for _, o := range options {
		o(opts)
	}
	b, _ := json.Marshal(opts)
	return string(b)
}

// Close frees the client's handle. Every later operation on it fails with
// kind io_error; Close itself is idempotent.
func (c *Client) Close() error {
	c.finalize()
	c.handle = 0
	runtime.SetFinalizer(c, nil)
	return nil
}

func (c *Client) finalize() {
	if c.handle != 0 {
		purego.SyscallN(c.lib.bcClientFree, c.handle)
	}
}

// open is the error every operation on a closed client returns.
func (c *Client) open() error {
	if c.handle == 0 {
		return &Error{Kind: "io_error", Message: "the client is closed", LibraryVersion: c.lib.version}
	}
	return nil
}

// GetFlag is one of the flags a Get takes.
type GetFlag uint32

const (
	// Force re-executes every Source the read demands before answering, as
	// comb get --force.
	Force GetFlag = 1 << 0 // BC_GET_FORCE
	// Wait re-executes a stale entry the read demands inline before
	// answering, as comb get --wait.
	Wait GetFlag = 1 << 1 // BC_GET_WAIT
)

// flagBits ORs flags into the bitmask bc_get and bc_session_get take.
func flagBits(flags []GetFlag) uintptr {
	var bits GetFlag
	for _, f := range flags {
		bits |= f
	}
	return uintptr(bits)
}

// Get returns the value comb get prints for key at cwd, with env as the
// caller's environment: a string, float64, bool, map[string]interface{} or
// []interface{}, as encoding/json decodes it, and nil on a miss. The daemon
// holds the templates ("<provider>:template", seeded from configuration or
// Put); this client renders them. A plain field is its cached value, a
// templated one is rendered, and a bare provider name is the whole merged
// namespace. The unrendered halves are facets, read like any key:
// "p:template.x" is the template as stored, "p:put.x" and "p:source.x"
// those layers' values.
//
// cwd is the directory the read addresses: "~" is the home directory and a
// relative path is against this process's working directory; "" is the
// global entry. env is the caller's environment for env.* references; nil
// means the calling process's environment, as comb has. flags are [Force] and
// [Wait].
func (c *Client) Get(key, cwd string, env map[string]string, flags ...GetFlag) (interface{}, error) {
	if err := c.open(); err != nil {
		return nil, err
	}
	keyBuf, cwdBuf, envBuf := cBytes(key), optBytes(cwd), jsonMapBytes(env)
	data, err := c.lib.call(c.lib.bcGet, c.handle, ptrOf(keyBuf), ptrOf(cwdBuf), ptrOf(envBuf), flagBits(flags))
	runtime.KeepAlive(keyBuf)
	runtime.KeepAlive(cwdBuf)
	runtime.KeepAlive(envBuf)
	if err != nil {
		return nil, err
	}
	v, err := decodeValue(data)
	return v, c.lib.stamp(err)
}

// Eval renders a template that is stored nowhere — the same value comb eval
// prints, decoded as Get's is, nil when it renders nothing. Every ref
// resolves exactly as it would inside a stored template, so
// Eval("{{ p.f }}") and Get("p.f") agree. cwd and env mean what they mean
// for Get: "" is the global entry.
//
// templateStr accepts a single "{{ expr }}" tag, or literal text/several
// tags — the former keeps the expression's natural type, the latter is
// always a string (a source with no tags at all is its literal text).
func (c *Client) Eval(templateStr, cwd string, env map[string]string) (interface{}, error) {
	if err := c.open(); err != nil {
		return nil, err
	}
	tplBuf, cwdBuf, envBuf := cBytes(templateStr), optBytes(cwd), jsonMapBytes(env)
	data, err := c.lib.call(c.lib.bcEval, c.handle, ptrOf(tplBuf), ptrOf(cwdBuf), ptrOf(envBuf))
	runtime.KeepAlive(tplBuf)
	runtime.KeepAlive(cwdBuf)
	runtime.KeepAlive(envBuf)
	if err != nil {
		return nil, err
	}
	v, err := decodeValue(data)
	return v, c.lib.stamp(err)
}

// Refresh forces the daemon to recompute the given provider/key at path;
// "" is the global entry.
func (c *Client) Refresh(key string, path string) error {
	if err := c.open(); err != nil {
		return err
	}
	keyBuf, pathBuf := cBytes(key), optBytes(path)
	_, err := c.lib.call(c.lib.bcRefresh, c.handle, ptrOf(keyBuf), ptrOf(pathBuf))
	runtime.KeepAlive(keyBuf)
	runtime.KeepAlive(pathBuf)
	return err
}

// Status returns cache rows from the daemon.
func (c *Client) Status() ([]CacheRow, error) {
	if err := c.open(); err != nil {
		return nil, err
	}
	data, err := c.lib.call(c.lib.bcStatus, c.handle)
	if err != nil {
		return nil, err
	}
	rows, err := parseCacheRowsFromData(data)
	return rows, c.lib.stamp(err)
}

// Session opens a persistent connection and returns a [Session].
func (c *Client) Session() (*Session, error) {
	if err := c.open(); err != nil {
		return nil, err
	}
	r1, _, _ := purego.SyscallN(c.lib.bcSessionOpen, c.handle)
	s := &Session{lib: c.lib, handle: r1}
	runtime.SetFinalizer(s, func(s *Session) { s.finalize() })
	return s, nil
}

// Hello returns the daemon's protocol and build versions.
func (c *Client) Hello() (*HelloInfo, error) {
	if err := c.open(); err != nil {
		return nil, err
	}
	data, err := c.lib.call(c.lib.bcHello, c.handle)
	if err != nil {
		return nil, err
	}
	info, err := parseHelloFromData(data)
	return info, c.lib.stamp(err)
}

// Put stores data into the cache as static data: a JSON object at a namespace
// root, any JSON below it. nil removes the node. ttl and path are optional;
// pass "" to omit them ("" path is the global entry).
func (c *Client) Put(key string, data interface{}, ttl, path string) error {
	if err := c.open(); err != nil {
		return err
	}
	dataJSON, err := json.Marshal(data)
	if err != nil {
		return c.lib.stamp(parseError("beachcomber: marshal put data: " + err.Error()))
	}
	keyBuf, dataBuf, ttlBuf, pathBuf := cBytes(key), cBytes(string(dataJSON)), optBytes(ttl), optBytes(path)
	_, err = c.lib.call(c.lib.bcPut, c.handle, ptrOf(keyBuf), ptrOf(dataBuf), ptrOf(ttlBuf), ptrOf(pathBuf))
	runtime.KeepAlive(keyBuf)
	runtime.KeepAlive(dataBuf)
	runtime.KeepAlive(ttlBuf)
	runtime.KeepAlive(pathBuf)
	return err
}

// Introspect runs a diagnostic query. A durationSecs, consulted by
// SubjectProcs only, is sent as given — 0 included; left out, the daemon's
// own default applies. Only the first is read.
func (c *Client) Introspect(subject IntrospectSubject, durationSecs ...uint64) (*IntrospectResponse, error) {
	if err := c.open(); err != nil {
		return nil, err
	}
	subjectBuf := cBytes(string(subject))
	var optsBuf []byte
	if len(durationSecs) > 0 {
		optsJSON, _ := json.Marshal(map[string]uint64{"duration_secs": durationSecs[0]})
		optsBuf = cBytes(string(optsJSON))
	}
	data, err := c.lib.call(c.lib.bcIntrospect, c.handle, ptrOf(subjectBuf), ptrOf(optsBuf))
	runtime.KeepAlive(subjectBuf)
	runtime.KeepAlive(optsBuf)
	if err != nil {
		return nil, err
	}
	resp, err := parseIntrospectFromData(subject, data)
	return resp, c.lib.stamp(err)
}

// Watch subscribes to the value of key — what Get answers for it — and
// emits it again whenever it changes, so a template edited under the watch
// fires it just as a changed cache record does. cwd and env mean what they
// mean for Get: "" is the global entry. The returned stream holds a watch
// handle; call Close on it to cancel and free it.
func (c *Client) Watch(key, cwd string, env map[string]string) (*WatchStream, error) {
	if err := c.open(); err != nil {
		return nil, err
	}
	keyBuf, cwdBuf, envBuf := cBytes(key), optBytes(cwd), jsonMapBytes(env)
	r1, _, _ := purego.SyscallN(c.lib.bcWatchOpen, c.handle, ptrOf(keyBuf), ptrOf(cwdBuf), ptrOf(envBuf))
	runtime.KeepAlive(keyBuf)
	runtime.KeepAlive(cwdBuf)
	runtime.KeepAlive(envBuf)
	if r1 == 0 {
		return nil, &Error{Kind: "io_error", Message: "beachcomber: bc_watch_open: allocation failure", LibraryVersion: c.lib.version}
	}
	w := &WatchStream{lib: c.lib, handle: r1}
	runtime.SetFinalizer(w, func(w *WatchStream) { w.finalize() })
	return w, nil
}
