package beachcomber

// Error is every failure this SDK returns: an ok:false envelope from the
// library, and the three kinds the SDK raises itself (library_not_found,
// library_symbol, unsupported). Kind is the stable slug the SDK API table
// (docs/sdk-api.md) names; Message is the library's words unchanged, or the
// SDK's own for its three; LibraryVersion is the loaded library's
// bc_version(), when one was loaded.
type Error struct {
	Kind           string
	Message        string
	LibraryVersion string
}

// Error is the message alone: the kind and the library version are fields
// of their own.
func (e *Error) Error() string {
	return e.Message
}

// Is reports whether target is an *Error of the same Kind, so
// errors.Is(err, ErrTimeout) asks whether err is a timeout.
func (e *Error) Is(target error) bool {
	t, ok := target.(*Error)
	return ok && t.Kind == e.Kind
}

// One sentinel per kind, matched by errors.Is on an *Error of that kind.
var (
	// ErrDaemonNotRunning: no daemon answers, and none was started.
	ErrDaemonNotRunning = &Error{Kind: "daemon_not_running", Message: "no daemon answers, and none was started"}
	// ErrConnectionFailed: an explicit socket path could not be connected to.
	ErrConnectionFailed = &Error{Kind: "connection_failed", Message: "the socket could not be connected to"}
	// ErrIo: a read or write on the socket failed.
	ErrIo = &Error{Kind: "io_error", Message: "a read or write on the socket failed"}
	// ErrParse: a request, a response or a template could not be read.
	ErrParse = &Error{Kind: "parse_error", Message: "a request, a response or a template could not be read"}
	// ErrServer: the daemon refused the request, or a template failed to render.
	ErrServer = &Error{Kind: "server_error", Message: "the daemon refused the request"}
	// ErrTimeout: a read or write ran out timeout_ms, or NextEvent ran out its timeout.
	ErrTimeout = &Error{Kind: "timeout", Message: "timed out"}
	// ErrBadFlags: a flag bit the ABI does not define was set.
	ErrBadFlags = &Error{Kind: "bad_flags", Message: "a flag bit the ABI does not define was set"}
	// ErrBusy: a session or watch is in use by another caller.
	ErrBusy = &Error{Kind: "busy", Message: "the handle is in use by another caller"}
	// ErrPanic: the library panicked; the panic was caught at the boundary.
	ErrPanic = &Error{Kind: "panic", Message: "the library panicked"}
	// ErrVersionSkew: the daemon is a different version from the library.
	ErrVersionSkew = &Error{Kind: "version_skew", Message: "the daemon is a different version from the library"}
	// ErrLibraryNotFound: no libbeachcomber was found where discovery looks.
	ErrLibraryNotFound = &Error{Kind: "library_not_found", Message: "libbeachcomber was not found"}
	// ErrLibrarySymbol: the library found lacks a function this SDK calls.
	ErrLibrarySymbol = &Error{Kind: "library_symbol", Message: "libbeachcomber lacks a function this SDK calls"}
	// ErrUnsupported: a transport cannot do this operation. This SDK has one
	// transport, the library, which can do every operation; the kind is here
	// so every SDK carries the same error kinds.
	ErrUnsupported = &Error{Kind: "unsupported", Message: "the transport cannot do this operation"}
)

// parseError is a response this SDK could not read.
func parseError(message string) *Error {
	return &Error{Kind: "parse_error", Message: message}
}
