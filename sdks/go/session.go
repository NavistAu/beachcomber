package beachcomber

import (
	"encoding/json"
	"runtime"

	"github.com/ebitengine/purego"
)

// Session holds a handle to a persistent daemon connection (bc_session_open).
// It is not safe for concurrent use from multiple goroutines: the underlying
// library guards it with a mutex and returns a "busy" *ServerError to a
// concurrent caller rather than blocking or interleaving requests — create
// one Session per goroutine instead.
//
// A session is obtained via [Client.Session]:
//
//	sess, err := client.Session()
//	if err != nil { … }
//	defer sess.Close()
type Session struct {
	// client backs Hello/Introspect/Status, which take no path and have no
	// session-level ABI equivalent: they fall back to a fresh connection via
	// the parent Client, exactly as if called directly on it.
	client *Client
	lib    *nativeLib
	handle uintptr // BcSession*
}

// Get is [Client.Get] on this session's persistent connection: the same
// rendered value, the same flags. cwd "" is this session's directory
// (SetContext) when it has one, else the global entry.
func (s *Session) Get(key, cwd string, env map[string]string, flags ...GetFlag) (*Result, error) {
	keyBuf, cwdBuf, envBuf := cBytes(key), optBytes(cwd), jsonMapBytes(env)
	data, err := s.lib.call(s.lib.bcSessionGet, s.handle, ptrOf(keyBuf), ptrOf(cwdBuf), ptrOf(envBuf), flagBits(flags))
	runtime.KeepAlive(keyBuf)
	runtime.KeepAlive(cwdBuf)
	runtime.KeepAlive(envBuf)
	if err != nil {
		return nil, err
	}
	return resultFromScalarData(data)
}

// Refresh forces the daemon to recompute the given provider/key at path, or
// at the session's directory (SetContext) when path is "".
func (s *Session) Refresh(key string, path string) error {
	keyBuf, pathBuf := cBytes(key), optBytes(path)
	_, err := s.lib.call(s.lib.bcSessionRefresh, s.handle, ptrOf(keyBuf), ptrOf(pathBuf))
	runtime.KeepAlive(keyBuf)
	runtime.KeepAlive(pathBuf)
	return err
}

// SetContext sets this session's directory: every later Get that passes cwd
// "", and Put and Refresh that pass path "", address it. The session keeps it
// and sends it with each request.
func (s *Session) SetContext(path string) error {
	pathBuf := cBytes(path)
	_, err := s.lib.call(s.lib.bcSessionSetContext, s.handle, ptrOf(pathBuf))
	runtime.KeepAlive(pathBuf)
	return err
}

// Hello returns the daemon's protocol and build versions. See the Session
// doc comment: this uses a fresh connection, not this session's.
func (s *Session) Hello() (*HelloInfo, error) {
	return s.client.Hello()
}

// Put stores data into the cache as static data on this session's persistent
// connection. data should be a JSON object. ttl and path are optional; pass
// "" to omit them.
func (s *Session) Put(key string, data interface{}, ttl, path string) error {
	dataJSON, err := json.Marshal(data)
	if err != nil {
		return &ProtocolError{msg: "marshal put data: " + err.Error()}
	}
	keyBuf, dataBuf, ttlBuf, pathBuf := cBytes(key), cBytes(string(dataJSON)), optBytes(ttl), optBytes(path)
	_, err = s.lib.call(s.lib.bcSessionPut, s.handle, ptrOf(keyBuf), ptrOf(dataBuf), ptrOf(ttlBuf), ptrOf(pathBuf))
	runtime.KeepAlive(keyBuf)
	runtime.KeepAlive(dataBuf)
	runtime.KeepAlive(ttlBuf)
	runtime.KeepAlive(pathBuf)
	return err
}

// Introspect runs a diagnostic query. See the Session doc comment: this uses
// a fresh connection, not this session's.
func (s *Session) Introspect(subject IntrospectSubject, durationSecs uint64) (*IntrospectResponse, error) {
	return s.client.Introspect(subject, durationSecs)
}

// Status returns cache rows from the daemon. See the Session doc comment:
// this uses a fresh connection, not this session's.
func (s *Session) Status() ([]CacheRow, error) {
	return s.client.Status()
}

// Close closes and frees the underlying session handle.
func (s *Session) Close() error {
	if s.handle != 0 {
		purego.SyscallN(s.lib.bcSessionClose, s.handle)
		s.handle = 0
		runtime.SetFinalizer(s, nil)
	}
	return nil
}

func (s *Session) finalize() {
	if s.handle != 0 {
		purego.SyscallN(s.lib.bcSessionClose, s.handle)
	}
}
