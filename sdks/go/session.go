package beachcomber

import (
	"encoding/json"
	"runtime"

	"github.com/ebitengine/purego"
)

// Session holds a handle to a persistent daemon connection (bc_session_open)
// and a directory of its own (SetContext). It is not safe for concurrent use
// from multiple goroutines: the underlying library guards it with a mutex
// and returns an *Error of kind busy to a concurrent caller rather than
// blocking or interleaving requests — create one Session per goroutine
// instead. Status, Hello and Introspect address no directory and need no
// connection of their own, so they are the Client's alone.
//
// A session is obtained via [Client.Session]:
//
//	sess, err := client.Session()
//	if err != nil { … }
//	defer sess.Close()
type Session struct {
	lib    *nativeLib
	handle uintptr // BcSession*; 0 once closed
}

// open is the error every operation on a closed session returns.
func (s *Session) open() error {
	if s.handle == 0 {
		return &Error{Kind: "io_error", Message: "the session is closed", LibraryVersion: s.lib.version}
	}
	return nil
}

// Get is [Client.Get] on this session's persistent connection: the same
// value, the same options. cwd "" is this session's directory (SetContext)
// when it has one, else the global entry.
func (s *Session) Get(key, cwd string, env map[string]string, options ...GetOption) (interface{}, error) {
	if err := s.open(); err != nil {
		return nil, err
	}
	keyBuf, cwdBuf, envBuf, optBuf := cBytes(key), optBytes(cwd), jsonMapBytes(env), getOptionsBytes(options)
	data, err := s.lib.call(s.lib.bcSessionGet, s.handle, ptrOf(keyBuf), ptrOf(cwdBuf), ptrOf(envBuf), ptrOf(optBuf))
	runtime.KeepAlive(keyBuf)
	runtime.KeepAlive(cwdBuf)
	runtime.KeepAlive(envBuf)
	runtime.KeepAlive(optBuf)
	if err != nil {
		return nil, err
	}
	v, err := decodeValue(data)
	return v, s.lib.stamp(err)
}

// Refresh forces the daemon to recompute the given provider/key at path, or
// at the session's directory (SetContext) when path is "".
func (s *Session) Refresh(key string, path string) error {
	if err := s.open(); err != nil {
		return err
	}
	keyBuf, pathBuf := cBytes(key), optBytes(path)
	_, err := s.lib.call(s.lib.bcSessionRefresh, s.handle, ptrOf(keyBuf), ptrOf(pathBuf))
	runtime.KeepAlive(keyBuf)
	runtime.KeepAlive(pathBuf)
	return err
}

// SetContext sets this session's directory: every later Get that passes cwd
// "", and every Put and Refresh that passes path "", addresses it. The
// session keeps it and sends it with each request.
func (s *Session) SetContext(path string) error {
	if err := s.open(); err != nil {
		return err
	}
	pathBuf := cBytes(path)
	_, err := s.lib.call(s.lib.bcSessionSetContext, s.handle, ptrOf(pathBuf))
	runtime.KeepAlive(pathBuf)
	return err
}

// Put stores data into the cache as static data on this session's persistent
// connection. data should be a JSON object. ttl and path are optional; pass
// "" to omit them (path "" is the session's directory, else the global
// entry).
func (s *Session) Put(key string, data interface{}, ttl, path string) error {
	if err := s.open(); err != nil {
		return err
	}
	dataJSON, err := json.Marshal(data)
	if err != nil {
		return s.lib.stamp(parseError("beachcomber: marshal put data: " + err.Error()))
	}
	keyBuf, dataBuf, ttlBuf, pathBuf := cBytes(key), cBytes(string(dataJSON)), optBytes(ttl), optBytes(path)
	_, err = s.lib.call(s.lib.bcSessionPut, s.handle, ptrOf(keyBuf), ptrOf(dataBuf), ptrOf(ttlBuf), ptrOf(pathBuf))
	runtime.KeepAlive(keyBuf)
	runtime.KeepAlive(dataBuf)
	runtime.KeepAlive(ttlBuf)
	runtime.KeepAlive(pathBuf)
	return err
}

// Close closes and frees the underlying session handle. Every later
// operation on it fails with kind io_error; Close itself is idempotent.
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
