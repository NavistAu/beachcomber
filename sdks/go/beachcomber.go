// Package beachcomber provides a client for the beachcomber daemon.
//
// The daemon caches shell-environment data (git state, hostname, battery, …)
// and serves it over a Unix domain socket using newline-delimited JSON.
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
	"bufio"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"time"
)

const defaultTimeout = 100 * time.Millisecond

// ErrDaemonNotRunning is returned when the Unix socket cannot be reached.
var ErrDaemonNotRunning = errors.New("beachcomber daemon is not running")

// ServerError is returned when the daemon responds with ok:false.
type ServerError struct {
	Message string
}

func (e *ServerError) Error() string {
	return fmt.Sprintf("beachcomber: daemon error: %s", e.Message)
}

// ProtocolError is returned when a response cannot be parsed.
type ProtocolError struct {
	msg string
}

func (e *ProtocolError) Error() string {
	return fmt.Sprintf("beachcomber: protocol error: %s", e.msg)
}

// Client sends individual requests, opening a fresh connection for each call.
// For latency-sensitive use cases prefer [Session].
type Client struct {
	socketPath string
	timeout    time.Duration
}

// NewClient returns a Client that auto-discovers the daemon socket path.
// Returns [ErrDaemonNotRunning] wrapped in an error if discovery fails (i.e.,
// none of the candidate paths resolve to a reachable socket). The error is
// deferred to the first actual network call; NewClient itself never dials.
func NewClient() (*Client, error) {
	path := DiscoverSocketPath()
	return &Client{
		socketPath: path,
		timeout:    defaultTimeout,
	}, nil
}

// NewClientWithPath returns a Client that uses an explicit socket path.
func NewClientWithPath(socketPath string) *Client {
	return &Client{
		socketPath: socketPath,
		timeout:    defaultTimeout,
	}
}

// Get reads a cached value from the daemon.
//
// key follows the "provider" or "provider.field" format (e.g. "git" or
// "git.branch"). path is the working directory context; pass "" to omit it.
func (c *Client) Get(key string, path string) (*Result, error) {
	req := map[string]interface{}{"op": "get", "key": key}
	if path != "" {
		req["path"] = path
	}
	return c.roundtrip(req)
}

// Refresh forces the daemon to recompute the given provider/key.
func (c *Client) Refresh(key string, path string) error {
	req := map[string]interface{}{"op": "refresh", "key": key}
	if path != "" {
		req["path"] = path
	}
	_, err := c.roundtrip(req)
	return err
}

// Status returns scheduler and cache status from the daemon.
func (c *Client) Status() (*Result, error) {
	return c.roundtrip(map[string]interface{}{"op": "status"})
}

// Session opens a persistent connection and returns a [Session].
func (c *Client) Session() (*Session, error) {
	conn, err := c.dial()
	if err != nil {
		return nil, err
	}
	return &Session{
		conn:    conn,
		scanner: bufio.NewScanner(conn),
	}, nil
}

// roundtrip dials, sends one request, reads one response, and closes.
func (c *Client) roundtrip(req map[string]interface{}) (*Result, error) {
	conn, err := c.dial()
	if err != nil {
		return nil, err
	}
	defer conn.Close()

	if err := writeJSON(conn, req); err != nil {
		return nil, err
	}

	scanner := bufio.NewScanner(conn)
	return readResponse(scanner)
}

func (c *Client) dial() (net.Conn, error) {
	conn, err := net.DialTimeout("unix", c.socketPath, c.timeout)
	if err != nil {
		return nil, fmt.Errorf("%w: %s", ErrDaemonNotRunning, c.socketPath)
	}
	return conn, nil
}

// writeJSON serialises req as a single JSON line.
func writeJSON(conn net.Conn, req map[string]interface{}) error {
	data, err := json.Marshal(req)
	if err != nil {
		return fmt.Errorf("beachcomber: marshal: %w", err)
	}
	data = append(data, '\n')
	_, err = conn.Write(data)
	return err
}

// readResponse reads one line from scanner and parses it as a daemon response.
func readResponse(scanner *bufio.Scanner) (*Result, error) {
	if !scanner.Scan() {
		if err := scanner.Err(); err != nil {
			return nil, fmt.Errorf("beachcomber: read: %w", err)
		}
		return nil, &ProtocolError{msg: "connection closed before response"}
	}
	return parseResponse(scanner.Bytes())
}
