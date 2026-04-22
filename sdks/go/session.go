package beachcomber

import (
	"bufio"
	"net"
)

// Session holds a persistent connection to the daemon. It is not safe for
// concurrent use; create one session per goroutine (or protect access with a
// mutex).
//
// A session is obtained via [Client.Session]:
//
//	sess, err := client.Session()
//	if err != nil { … }
//	defer sess.Close()
type Session struct {
	conn    net.Conn
	scanner *bufio.Scanner
}

// Get reads a cached value. path="" omits the path field.
func (s *Session) Get(key string, path string) (*Result, error) {
	req := map[string]interface{}{"op": "get", "key": key}
	if path != "" {
		req["path"] = path
	}
	return s.roundtrip(req)
}

// Refresh forces the daemon to recompute the given provider/key.
func (s *Session) Refresh(key string, path string) error {
	req := map[string]interface{}{"op": "refresh", "key": key}
	if path != "" {
		req["path"] = path
	}
	_, err := s.roundtrip(req)
	return err
}

// SetContext sends a "context" message, which sets the default path for all
// subsequent queries on this connection. This avoids repeating the path in
// every Get/Refresh call.
func (s *Session) SetContext(path string) error {
	_, err := s.roundtrip(map[string]interface{}{"op": "context", "path": path})
	return err
}

// GetWithFlags is like Get but supports force and wait flags.
func (s *Session) GetWithFlags(key, path string, force, wait bool) (*Result, error) {
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
	return s.roundtrip(req)
}

// Hello returns the daemon's protocol and build versions.
func (s *Session) Hello() (*HelloInfo, error) {
	result, err := s.roundtrip(map[string]interface{}{"op": "hello"})
	if err != nil {
		return nil, err
	}
	return parseHelloFromResult(result)
}

// Put stores data into a virtual provider. data should be a JSON object.
// ttl and path are optional; pass "" to omit them.
func (s *Session) Put(key string, data interface{}, ttl, path string) error {
	req := map[string]interface{}{"op": "put", "key": key, "data": data}
	if ttl != "" {
		req["ttl"] = ttl
	}
	if path != "" {
		req["path"] = path
	}
	_, err := s.roundtrip(req)
	return err
}

// Introspect runs a diagnostic query. durationSecs is only consulted for
// SubjectProcs; pass 0 otherwise.
func (s *Session) Introspect(subject IntrospectSubject, durationSecs uint64) (*IntrospectResponse, error) {
	req := map[string]interface{}{"op": "introspect", "subject": string(subject)}
	if durationSecs > 0 {
		req["duration_secs"] = durationSecs
	}
	result, err := s.roundtrip(req)
	if err != nil {
		return nil, err
	}
	return parseIntrospectFromResult(subject, result)
}

// StatusRows returns the typed cache-row array from Status.
func (s *Session) StatusRows() ([]CacheRow, error) {
	result, err := s.roundtrip(map[string]interface{}{"op": "status"})
	if err != nil {
		return nil, err
	}
	return parseCacheRowsFromResult(result)
}

// Close closes the underlying connection.
func (s *Session) Close() error {
	return s.conn.Close()
}

func (s *Session) roundtrip(req map[string]interface{}) (*Result, error) {
	if err := writeJSON(s.conn, req); err != nil {
		return nil, err
	}
	return readResponse(s.scanner)
}
