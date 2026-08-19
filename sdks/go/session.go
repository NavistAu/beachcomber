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
	// client backs Refresh/Hello/Introspect/Status: the ABI exposes only
	// bc_session_get/put/set_context on a session's persistent connection
	// (libbeachcomber-ffi/include/beachcomber.h), so those four fall back
	// to a fresh connection via the parent Client, exactly as if called
	// directly on it. Results are identical; only the "reuses this
	// session's connection" performance property doesn't hold for them.
	client *Client
	lib    *nativeLib
	handle uintptr // BcSession*
}

// Get reads a cached value. path="" omits the path field.
func (s *Session) Get(key string, path string) (*Result, error) {
	return s.GetWithFlags(key, path, false, false)
}

// GetWithFlags is like Get but supports force and wait flags.
func (s *Session) GetWithFlags(key, path string, force, wait bool) (*Result, error) {
	keyBuf, pathBuf := cBytes(key), optBytes(path)
	data, err := s.lib.call(s.lib.bcSessionGet, s.handle, ptrOf(keyBuf), ptrOf(pathBuf), uintptr(getFlags(force, wait)))
	runtime.KeepAlive(keyBuf)
	runtime.KeepAlive(pathBuf)
	if err != nil {
		return nil, err
	}
	return resultFromGetData(data)
}

// Refresh forces the daemon to recompute the given provider/key. See the
// Session doc comment: this uses a fresh connection, not this session's.
func (s *Session) Refresh(key string, path string) error {
	return s.client.Refresh(key, path)
}

// SetContext sends a "context" message, which sets the default path for all
// subsequent queries on this connection. This avoids repeating the path in
// every Get call.
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

// Put stores data into a virtual provider on this session's persistent
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
