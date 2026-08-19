package beachcomber

// Transport: this SDK binds to libbeachcomber's C ABI (see
// libbeachcomber-ffi/include/beachcomber.h) via purego — a pure-Go dynamic
// loader (dlopen/dlsym + a hand-rolled calling convention), not cgo. This
// keeps CGO_ENABLED=0 builds working, at the cost of taking on
// github.com/ebitengine/purego as this SDK's first non-stdlib dependency
// (every other beachcomber SDK is stdlib-only). purego was chosen because
// the entire ABI surface fits its supported shape: every function takes
// pointer/integer arguments and returns either a pointer or a small
// integer — no callbacks into Go, no varargs, no structs passed by value.
// A cgo binding was the documented fallback had that not held; it does, so
// this file never touches cgo.

import (
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"unsafe"

	"github.com/ebitengine/purego"
)

// requiredSymbols lists every bc_* entry point this binding calls, other
// than bc_version (resolved separately, best-effort, so it can be reported
// even when a later symbol is missing).
var requiredSymbolNames = []string{
	"bc_client_new", "bc_client_free",
	"bc_get", "bc_put", "bc_put_null", "bc_refresh", "bc_status", "bc_introspect", "bc_hello",
	"bc_resolve", "bc_eval",
	"bc_session_open", "bc_session_close", "bc_session_get", "bc_session_put", "bc_session_set_context",
	"bc_watch_open", "bc_watch_next", "bc_watch_cancel", "bc_watch_free",
	"bc_string_free",
}

// nativeLib holds the dlopen handle and every symbol address this binding
// calls, resolved once at load time (contract point 3: required symbols are
// checked at load, not on first use).
type nativeLib struct {
	handle  uintptr
	path    string
	version string

	bcClientNew, bcClientFree                                  uintptr
	bcGet, bcPut, bcPutNull, bcRefresh, bcStatus, bcIntrospect uintptr
	bcHello, bcResolve, bcEval                                 uintptr
	bcSessionOpen, bcSessionClose, bcSessionGet                uintptr
	bcSessionPut, bcSessionSetContext                          uintptr
	bcWatchOpen, bcWatchNext, bcWatchCancel, bcWatchFree       uintptr
	bcStringFree                                               uintptr
}

var (
	libOnce sync.Once
	lib     *nativeLib
	libErr  error
)

// getLib loads and validates libbeachcomber exactly once per process.
func getLib() (*nativeLib, error) {
	libOnce.Do(func() {
		lib, libErr = loadNativeLib()
	})
	return lib, libErr
}

// libraryFileName returns the platform-specific artifact name libbeachcomber
// ships as (see the plan's Naming section — the crate and the file share the
// name "libbeachcomber").
func libraryFileName() string {
	if runtime.GOOS == "darwin" {
		return "libbeachcomber.dylib"
	}
	return "libbeachcomber.so"
}

// candidateLibraryPaths returns the ordered discovery candidates: (1)
// $BEACHCOMBER_LIB, (2) ../lib/ relative to the resolved `comb` on $PATH,
// (3) the platform default dynamic-linker search path (a bare filename, left
// for dlopen itself to resolve). The comb-relative location deliberately
// comes before the system path: library and binary ship together, so the
// one beside the comb you would actually run should win over a stale
// /usr/lib copy.
func candidateLibraryPaths() []string {
	var candidates []string
	if v := os.Getenv("BEACHCOMBER_LIB"); v != "" {
		candidates = append(candidates, v)
	}
	if combPath, err := exec.LookPath("comb"); err == nil {
		if resolved, err := filepath.EvalSymlinks(combPath); err == nil {
			combPath = resolved
		}
		candidates = append(candidates, filepath.Join(filepath.Dir(combPath), "..", "lib", libraryFileName()))
	}
	candidates = append(candidates, libraryFileName())
	return candidates
}

// loadNativeLib performs discovery (contract point 1), fails loudly naming
// every location tried (point 2), and checks every required symbol before
// returning a usable handle (point 3).
func loadNativeLib() (*nativeLib, error) {
	var attempts []string
	var handle uintptr
	var loadedPath string
	for _, path := range candidateLibraryPaths() {
		h, err := purego.Dlopen(path, purego.RTLD_NOW|purego.RTLD_GLOBAL)
		if err != nil {
			attempts = append(attempts, fmt.Sprintf("%s (%v)", path, err))
			continue
		}
		handle, loadedPath = h, path
		break
	}
	if handle == 0 {
		return nil, &LibraryError{Message: fmt.Sprintf(
			"beachcomber: could not locate libbeachcomber; tried, in order: %s",
			strings.Join(attempts, "; "),
		)}
	}

	l := &nativeLib{handle: handle, path: loadedPath}

	// bc_version is resolved and called first, best-effort, so it can be
	// named in any error produced by the symbol check below.
	if addr, err := purego.Dlsym(handle, "bc_version"); err == nil {
		r1, _, _ := purego.SyscallN(addr)
		l.version = cStringToGo(r1) // static string: never freed
	}

	dst := map[string]*uintptr{
		"bc_client_new": &l.bcClientNew, "bc_client_free": &l.bcClientFree,
		"bc_get": &l.bcGet, "bc_put": &l.bcPut, "bc_put_null": &l.bcPutNull,
		"bc_refresh": &l.bcRefresh, "bc_status": &l.bcStatus, "bc_introspect": &l.bcIntrospect,
		"bc_hello": &l.bcHello, "bc_resolve": &l.bcResolve, "bc_eval": &l.bcEval,
		"bc_session_open": &l.bcSessionOpen, "bc_session_close": &l.bcSessionClose,
		"bc_session_get": &l.bcSessionGet, "bc_session_put": &l.bcSessionPut,
		"bc_session_set_context": &l.bcSessionSetContext,
		"bc_watch_open":          &l.bcWatchOpen, "bc_watch_next": &l.bcWatchNext,
		"bc_watch_cancel": &l.bcWatchCancel, "bc_watch_free": &l.bcWatchFree,
		"bc_string_free": &l.bcStringFree,
	}
	var missing []string
	for _, name := range requiredSymbolNames {
		addr, err := purego.Dlsym(handle, name)
		if err != nil {
			missing = append(missing, name)
			continue
		}
		*dst[name] = addr
	}
	if len(missing) > 0 {
		return nil, &LibraryError{
			Message:    fmt.Sprintf("beachcomber: %s is missing required symbol(s): %s", loadedPath, strings.Join(missing, ", ")),
			LibVersion: l.version,
		}
	}
	return l, nil
}

// ---------------------------------------------------------------------------
// C string marshalling
// ---------------------------------------------------------------------------

// cBytes returns a NUL-terminated byte slice for s. The caller must keep the
// returned slice alive (runtime.KeepAlive) until the FFI call it is passed to
// has returned.
func cBytes(s string) []byte {
	b := make([]byte, len(s)+1)
	copy(b, s)
	return b
}

// optBytes is like cBytes but returns nil for "", which ptrOf maps to a NULL
// pointer — this SDK's existing convention for "field omitted".
func optBytes(s string) []byte {
	if s == "" {
		return nil
	}
	return cBytes(s)
}

// ptrOf returns the address of b's first byte, or 0 (NULL) for a nil/empty
// slice.
func ptrOf(b []byte) uintptr {
	if len(b) == 0 {
		return 0
	}
	return uintptr(unsafe.Pointer(&b[0]))
}

// i32arg encodes an int32 (e.g. bc_watch_next's timeout_ms) into the low 32
// bits of a uintptr call argument; the C callee reads only those bits.
func i32arg(v int32) uintptr {
	return uintptr(uint32(v))
}

// cStringToGo copies a NUL-terminated C string at ptr into a Go string
// without taking ownership of it. ptr == 0 (NULL) yields "".
func cStringToGo(ptr uintptr) string {
	if ptr == 0 {
		return ""
	}
	base := unsafe.Pointer(ptr)
	n := 0
	for *(*byte)(unsafe.Add(base, n)) != 0 {
		n++
	}
	return string(unsafe.Slice((*byte)(base), n))
}

// jsonMapBytes marshals m to a JSON object, or returns nil (NULL) when m is
// nil — the ABI's "not supplied" sentinel for env_json/overrides_json.
func jsonMapBytes(m map[string]string) []byte {
	if m == nil {
		return nil
	}
	b, _ := json.Marshal(m) // map[string]string always marshals successfully
	return cBytes(string(b))
}

// ---------------------------------------------------------------------------
// Envelope decoding — every bc_* op function funnels through here
// ---------------------------------------------------------------------------

type wireError struct {
	Kind    string `json:"kind"`
	Message string `json:"message"`
}

type wireEnvelope struct {
	OK    bool            `json:"ok"`
	Data  json.RawMessage `json:"data,omitempty"`
	Error *wireError      `json:"error,omitempty"`
}

// takeString reads the NUL-terminated string ptr points to and frees it via
// bc_string_free. Never call this on bc_version()'s return value.
func (l *nativeLib) takeString(ptr uintptr) string {
	if ptr == 0 {
		return ""
	}
	s := cStringToGo(ptr)
	purego.SyscallN(l.bcStringFree, ptr)
	return s
}

// call invokes the bc_* function at fnAddr, frees its returned string, and
// decodes the ordinary {"ok":...} envelope. ok:false becomes a *ServerError
// carrying the envelope's machine-readable Kind and this library's version.
func (l *nativeLib) call(fnAddr uintptr, args ...uintptr) (json.RawMessage, error) {
	r1, _, _ := purego.SyscallN(fnAddr, args...)
	s := l.takeString(r1)
	var env wireEnvelope
	if err := json.Unmarshal([]byte(s), &env); err != nil {
		return nil, &ProtocolError{msg: "malformed envelope: " + err.Error()}
	}
	if !env.OK {
		kind, msg := "", s
		if env.Error != nil {
			kind, msg = env.Error.Kind, env.Error.Message
		}
		return nil, &ServerError{Kind: kind, Message: msg, LibVersion: l.version}
	}
	return env.Data, nil
}

// watchEnvelope is bc_watch_next's five-outcome envelope shape (see
// envelope::WatchOutcome in libbeachcomber-ffi): event/timeout/eof/cancelled
// via ok:true plus Outcome, or the ordinary ok:false error envelope.
type watchEnvelope struct {
	OK      bool            `json:"ok"`
	Outcome string          `json:"outcome,omitempty"`
	Data    json.RawMessage `json:"data,omitempty"`
	Error   *wireError      `json:"error,omitempty"`
}

func (l *nativeLib) callWatch(fnAddr uintptr, args ...uintptr) (watchEnvelope, error) {
	r1, _, _ := purego.SyscallN(fnAddr, args...)
	s := l.takeString(r1)
	var env watchEnvelope
	if err := json.Unmarshal([]byte(s), &env); err != nil {
		return watchEnvelope{}, &ProtocolError{msg: "malformed watch envelope: " + err.Error()}
	}
	return env, nil
}
