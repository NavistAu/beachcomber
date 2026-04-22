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
