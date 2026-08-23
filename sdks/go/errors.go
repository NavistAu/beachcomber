package beachcomber

import (
	"errors"
	"fmt"
)

// ErrDaemonNotRunning is returned when the Unix socket cannot be reached.
// A *ServerError whose Kind is "daemon_not_running" reports true from
// errors.Is(err, ErrDaemonNotRunning).
var ErrDaemonNotRunning = errors.New("beachcomber daemon is not running")

// ServerError is returned when the library reports an ok:false envelope —
// either the daemon rejected the request, or the ABI itself did (bad flags,
// a busy handle, a caught panic, version skew). Kind is the envelope's
// stable, machine-readable slug (see libbeachcomber-ffi/src/envelope.rs);
// LibVersion is the loaded library's bc_version(), included per this SDK's
// binding contract so every error names the library that produced it.
type ServerError struct {
	Kind       string
	Message    string
	LibVersion string
}

func (e *ServerError) Error() string {
	if e.Kind == "" {
		return fmt.Sprintf("beachcomber: daemon error: %s (library %s)", e.Message, e.LibVersion)
	}
	return fmt.Sprintf("beachcomber: %s: %s (library %s)", e.Kind, e.Message, e.LibVersion)
}

// Is reports whether target is ErrDaemonNotRunning and this error's Kind
// means the daemon could not be reached — "daemon_not_running" (no override
// socket path, none found by auto-discovery) or "connection_failed" (an
// explicit socket path, e.g. via NewClientWithPath, that nothing is
// listening on). Both were a single case in this SDK before the ABI made
// the distinction; folding them back together here keeps existing
// errors.Is(err, beachcomber.ErrDaemonNotRunning) checks working regardless
// of which construction path produced the Client.
func (e *ServerError) Is(target error) bool {
	return target == ErrDaemonNotRunning && (e.Kind == "daemon_not_running" || e.Kind == "connection_failed")
}

// ProtocolError is returned when a response cannot be parsed — malformed
// JSON returned by the library, or a response missing fields this SDK
// requires. Unlike ServerError this never comes from an ok:false envelope.
type ProtocolError struct {
	msg string
}

func (e *ProtocolError) Error() string {
	return fmt.Sprintf("beachcomber: protocol error: %s", e.msg)
}

// LibraryError reports a failure to locate, load, or validate
// libbeachcomber itself — discovery exhausted every candidate location, or
// the library that did load is missing a required symbol. Message already
// names every location tried (discovery failures) or the missing symbol
// (validation failures); LibVersion is populated when bc_version() could
// still be read despite the failure.
type LibraryError struct {
	Message    string
	LibVersion string
}

func (e *LibraryError) Error() string {
	if e.LibVersion != "" {
		return fmt.Sprintf("%s (loaded library version: %s)", e.Message, e.LibVersion)
	}
	return e.Message
}
