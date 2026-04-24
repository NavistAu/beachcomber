// Conformance runner for the beachcomber Go SDK.
//
// Loads every *.json fixture under tests/conformance/, starts a fresh daemon
// per fixture, drives the Go SDK's public API, and asserts structural
// expectations.
//
// Usage:
//
//	COMB_BIN=/path/to/comb go run ./cmd/conformance
//
// If COMB_BIN is unset, the runner looks for ../../target/debug/comb relative
// to the Go SDK directory (i.e. the repo's debug build).
package main

import (
	"encoding/json"
	"fmt"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"time"

	beachcomber "github.com/NavistAu/beachcomber/sdks/go"
)

// ---------------------------------------------------------------------------
// Fixture types
// ---------------------------------------------------------------------------

type opDescriptor struct {
	Op   string                 `json:"op"`
	Args map[string]interface{} `json:"args"`
}

type expectBlock struct {
	Status            string      `json:"status"`       // hit|miss|ok|error
	DataType          string      `json:"data_type"`    // string|number|bool|object|array|null
	DataEquals        interface{} `json:"data_equals"`  // exact deep-equality
	DataAsText        string      `json:"data_as_text"` // scalar stringification
	DataContainsField string      `json:"data_contains_field"`
	DataFieldEquals   *struct {
		Field string      `json:"field"`
		Value interface{} `json:"value"`
	} `json:"data_field_equals"`
	AgeMsPresent  *bool  `json:"age_ms_present"`
	Stale         *bool  `json:"stale"`
	ErrorContains string `json:"error_contains"`
}

type fixture struct {
	Name        string         `json:"name"`
	Description string         `json:"description"`
	Setup       []opDescriptor `json:"setup"`
	Test        opDescriptor   `json:"test"`
	Expect      expectBlock    `json:"expect"`
	path        string         // source file path (not in JSON)
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

func main() {
	combBin := os.Getenv("COMB_BIN")
	if combBin == "" {
		// Resolve relative to the Go SDK directory (this file's location).
		_, thisFile, _, _ := runtime.Caller(0)
		sdkDir := filepath.Join(filepath.Dir(thisFile), "..", "..")
		combBin = filepath.Join(sdkDir, "..", "..", "target", "debug", "comb")
	}

	// Walk up from cwd to find the repo root (contains tests/conformance/README.md).
	conformanceDir, err := findConformanceDir()
	if err != nil {
		fmt.Fprintf(os.Stderr, "conformance: %v\n", err)
		os.Exit(1)
	}

	fixtures, err := loadFixtures(conformanceDir)
	if err != nil {
		fmt.Fprintf(os.Stderr, "conformance: load fixtures: %v\n", err)
		os.Exit(1)
	}

	if len(fixtures) == 0 {
		fmt.Fprintln(os.Stderr, "conformance: no fixtures found")
		os.Exit(1)
	}

	passed, failed := 0, 0
	for _, f := range fixtures {
		ok, msg := runFixture(f, combBin)
		if ok {
			fmt.Printf("[PASS] %s\n", f.Name)
			passed++
		} else {
			fmt.Printf("[FAIL] %s — %s\n", f.Name, msg)
			failed++
		}
	}

	fmt.Printf("\n%d/%d fixtures passed\n", passed, passed+failed)
	if failed > 0 {
		os.Exit(1)
	}
}

// ---------------------------------------------------------------------------
// Fixture discovery
// ---------------------------------------------------------------------------

func findConformanceDir() (string, error) {
	// Walk up from cwd looking for tests/conformance/README.md.
	dir, err := os.Getwd()
	if err != nil {
		return "", err
	}
	for {
		candidate := filepath.Join(dir, "tests", "conformance", "README.md")
		if _, err := os.Stat(candidate); err == nil {
			return filepath.Join(dir, "tests", "conformance"), nil
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			break
		}
		dir = parent
	}
	return "", fmt.Errorf("could not find tests/conformance/README.md by walking up from cwd")
}

func loadFixtures(dir string) ([]fixture, error) {
	var fixtures []fixture
	err := walkDir(dir, func(path string) error {
		if !strings.HasSuffix(path, ".json") {
			return nil
		}
		data, err := os.ReadFile(path)
		if err != nil {
			return err
		}
		var f fixture
		if err := json.Unmarshal(data, &f); err != nil {
			return fmt.Errorf("%s: %w", path, err)
		}
		f.path = path
		fixtures = append(fixtures, f)
		return nil
	})
	return fixtures, err
}

// walkDir recursively visits all files under dir and calls fn with each path.
func walkDir(dir string, fn func(string) error) error {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return err
	}
	for _, e := range entries {
		p := filepath.Join(dir, e.Name())
		if e.IsDir() {
			if err := walkDir(p, fn); err != nil {
				return err
			}
		} else {
			if err := fn(p); err != nil {
				return err
			}
		}
	}
	return nil
}

// ---------------------------------------------------------------------------
// Daemon lifecycle
// ---------------------------------------------------------------------------

func startDaemon(combBin, socketPath string) (*exec.Cmd, error) {
	cmd := exec.Command(combBin, "daemon", "--socket", socketPath)
	cmd.Stdout = os.Stderr // daemon logs to stderr for visibility
	cmd.Stderr = os.Stderr
	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("start daemon: %w", err)
	}

	// Wait until the socket appears (up to 5s).
	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		if conn, err := net.DialTimeout("unix", socketPath, 100*time.Millisecond); err == nil {
			conn.Close()
			return cmd, nil
		}
		time.Sleep(50 * time.Millisecond)
	}
	cmd.Process.Kill()
	return nil, fmt.Errorf("daemon did not start within 5s on %s", socketPath)
}

func stopDaemon(cmd *exec.Cmd) {
	if cmd == nil || cmd.Process == nil {
		return
	}
	cmd.Process.Kill()
	cmd.Wait()
}

// ---------------------------------------------------------------------------
// Fixture execution
// ---------------------------------------------------------------------------

func runFixture(f fixture, combBin string) (bool, string) {
	// Create a temp socket path.
	tmpDir, err := os.MkdirTemp("", "bcconf-")
	if err != nil {
		return false, "mktemp: " + err.Error()
	}
	defer os.RemoveAll(tmpDir)
	sockPath := filepath.Join(tmpDir, "sock")

	cmd, err := startDaemon(combBin, sockPath)
	if err != nil {
		return false, "daemon: " + err.Error()
	}
	defer stopDaemon(cmd)

	client := beachcomber.NewClientWithPath(sockPath)

	// Run setup ops (no expectations checked).
	for _, op := range f.Setup {
		if err := runOp(client, op, nil, &f.Expect); err != nil {
			// Setup errors are non-fatal only if the test op will reveal them.
			_ = err
		}
	}

	// Run the test op and collect the result.
	result, watchEvent, serverErr := runTestOp(client, f.Test)
	return checkExpect(f.Expect, result, watchEvent, serverErr)
}

// opResult holds the SDK result for a non-watch op.
type opResult struct {
	data     interface{}
	ageMs    uint64
	ageMsSet bool // true when daemon sent age_ms (non-zero or explicitly 0 is ambiguous — use >0)
	stale    bool
}

// runOp executes a setup op. If expect is non-nil (for error ops in setup) it
// is ignored here. Returns error only for fatal dial/write problems.
func runOp(c *beachcomber.Client, op opDescriptor, _ *opResult, _ *expectBlock) error {
	switch op.Op {
	case "put":
		key, _ := op.Args["key"].(string)
		data := op.Args["data"]
		ttl, _ := op.Args["ttl"].(string)
		path, _ := op.Args["path"].(string)
		return c.Put(key, data, ttl, path)
	case "get":
		key, _ := op.Args["key"].(string)
		path, _ := op.Args["path"].(string)
		_, err := c.Get(key, path)
		return err
	case "refresh":
		key, _ := op.Args["key"].(string)
		path, _ := op.Args["path"].(string)
		return c.Refresh(key, path)
	case "context":
		// SetContext requires a session; for setup we open a one-shot session.
		sess, err := c.Session()
		if err != nil {
			return err
		}
		defer sess.Close()
		path, _ := op.Args["path"].(string)
		return sess.SetContext(path)
	}
	return nil
}

// runTestOp executes the test op and returns:
//   - result (*opResult) when the op returns data
//   - watchEvent (*beachcomber.WatchEvent) for watch ops
//   - serverErr (error) when the daemon returns an error
func runTestOp(c *beachcomber.Client, op opDescriptor) (*opResult, *beachcomber.WatchEvent, error) {
	switch op.Op {
	case "hello":
		info, err := c.Hello()
		if err != nil {
			return nil, nil, err
		}
		m := map[string]interface{}{
			"protocol_version": info.ProtocolVersion,
			"daemon_version":   info.DaemonVersion,
		}
		return &opResult{data: m}, nil, nil

	case "get":
		key, _ := op.Args["key"].(string)
		path, _ := op.Args["path"].(string)
		r, err := c.Get(key, path)
		if err != nil {
			return nil, nil, err
		}
		return resultFromSDK(r), nil, nil

	case "refresh":
		key, _ := op.Args["key"].(string)
		path, _ := op.Args["path"].(string)
		err := c.Refresh(key, path)
		if err != nil {
			return nil, nil, err
		}
		return &opResult{}, nil, nil

	case "put":
		key, _ := op.Args["key"].(string)
		data := op.Args["data"]
		ttl, _ := op.Args["ttl"].(string)
		path, _ := op.Args["path"].(string)
		err := c.Put(key, data, ttl, path)
		if err != nil {
			return nil, nil, err
		}
		return &opResult{}, nil, nil

	case "status":
		rows, err := c.Status()
		if err != nil {
			return nil, nil, err
		}
		data := make([]interface{}, len(rows))
		for i, row := range rows {
			data[i] = row
		}
		return &opResult{data: data}, nil, nil

	case "context":
		sess, err := c.Session()
		if err != nil {
			return nil, nil, err
		}
		defer sess.Close()
		p, _ := op.Args["path"].(string)
		if err := sess.SetContext(p); err != nil {
			return nil, nil, err
		}
		return &opResult{}, nil, nil

	case "introspect":
		subjectStr, _ := op.Args["subject"].(string)
		subject := beachcomber.IntrospectSubject(subjectStr)
		var durSecs uint64
		if d, ok := op.Args["duration_secs"].(float64); ok {
			durSecs = uint64(d)
		}
		resp, err := c.Introspect(subject, durSecs)
		if err != nil {
			return nil, nil, err
		}
		var data interface{}
		if resp.Daemon != nil {
			// Expose typed DaemonHealth as a map so checkExpect can use data_contains_field.
			data = daemonHealthToMap(resp.Daemon)
		} else {
			data = resp.Other
		}
		return &opResult{data: data}, nil, nil

	case "watch":
		key, _ := op.Args["key"].(string)
		path, _ := op.Args["path"].(string)
		ws, err := c.Watch(key, path)
		if err != nil {
			return nil, nil, err
		}
		defer ws.Close()
		ev, err := ws.NextEvent()
		if err != nil {
			return nil, nil, err
		}
		return nil, ev, nil
	}

	return nil, nil, fmt.Errorf("unknown op: %s", op.Op)
}

func resultFromSDK(r *beachcomber.Result) *opResult {
	or := &opResult{
		data:  r.Data,
		stale: r.Stale,
	}
	if r.AgeMs > 0 {
		or.ageMs = r.AgeMs
		or.ageMsSet = true
	}
	return or
}

func daemonHealthToMap(d *beachcomber.DaemonHealth) map[string]interface{} {
	m := map[string]interface{}{
		"pid":             d.PID,
		"version":         d.Version,
		"uptime_secs":     d.UptimeSecs,
		"socket_path":     d.SocketPath,
		"requests_total":  d.RequestsTotal,
		"in_flight":       d.InFlight,
		"active_watchers": d.ActiveWatchers,
		"cache_entries":   d.CacheEntries,
	}
	if d.ConfigPath != "" {
		m["config_path"] = d.ConfigPath
	}
	verdicts := make([]interface{}, len(d.Verdicts))
	for i, v := range d.Verdicts {
		verdicts[i] = map[string]interface{}{"level": v.Level, "message": v.Message}
	}
	m["verdicts"] = verdicts
	return m
}

// ---------------------------------------------------------------------------
// Expectation checking
// ---------------------------------------------------------------------------

func checkExpect(exp expectBlock, r *opResult, we *beachcomber.WatchEvent, serverErr error) (bool, string) {
	// Unify the two result forms into one common shape for assertion.
	var data interface{}
	var ageMs uint64
	var ageMsSet bool
	var stale bool
	isError := serverErr != nil

	if we != nil {
		data = we.Data
		ageMs = we.AgeMs
		ageMsSet = we.AgeMs > 0
		stale = we.Stale
	} else if r != nil {
		data = r.data
		ageMs = r.ageMs
		ageMsSet = r.ageMsSet
		stale = r.stale
	}

	// status assertion
	switch exp.Status {
	case "hit":
		if isError {
			return false, "expected hit, got error: " + serverErr.Error()
		}
		if data == nil {
			return false, "expected hit (data present), got miss"
		}
	case "miss":
		if isError {
			return false, "expected miss, got error: " + serverErr.Error()
		}
		if data != nil {
			return false, fmt.Sprintf("expected miss (no data), got data: %v", data)
		}
	case "ok":
		if isError {
			return false, "expected ok, got error: " + serverErr.Error()
		}
	case "error":
		if !isError {
			return false, fmt.Sprintf("expected error, got ok (data=%v)", data)
		}
		if exp.ErrorContains != "" {
			if !strings.Contains(serverErr.Error(), exp.ErrorContains) {
				return false, fmt.Sprintf("error %q does not contain %q", serverErr.Error(), exp.ErrorContains)
			}
		}
		return true, ""
	case "":
		// no status assertion
	}

	if isError {
		return false, "unexpected error: " + serverErr.Error()
	}

	// data_type assertion
	if exp.DataType != "" {
		got := jsonType(data)
		if got != exp.DataType {
			return false, fmt.Sprintf("data_type: got %q, want %q", got, exp.DataType)
		}
	}

	// data_equals assertion
	if exp.DataEquals != nil {
		if !deepEqual(data, exp.DataEquals) {
			return false, fmt.Sprintf("data_equals: got %v, want %v", data, exp.DataEquals)
		}
	}

	// data_as_text assertion
	if exp.DataAsText != "" {
		text := toText(data)
		if text != exp.DataAsText {
			return false, fmt.Sprintf("data_as_text: got %q, want %q", text, exp.DataAsText)
		}
	}

	// data_contains_field assertion
	if exp.DataContainsField != "" {
		m, ok := data.(map[string]interface{})
		if !ok {
			return false, fmt.Sprintf("data_contains_field: data is not an object, got %T", data)
		}
		if _, has := m[exp.DataContainsField]; !has {
			return false, fmt.Sprintf("data_contains_field: field %q absent in %v", exp.DataContainsField, data)
		}
	}

	// data_field_equals assertion
	if exp.DataFieldEquals != nil {
		m, ok := data.(map[string]interface{})
		if !ok {
			return false, fmt.Sprintf("data_field_equals: data is not an object, got %T", data)
		}
		val, has := m[exp.DataFieldEquals.Field]
		if !has {
			return false, fmt.Sprintf("data_field_equals: field %q absent", exp.DataFieldEquals.Field)
		}
		if !deepEqual(val, exp.DataFieldEquals.Value) {
			return false, fmt.Sprintf("data_field_equals: field %q = %v, want %v", exp.DataFieldEquals.Field, val, exp.DataFieldEquals.Value)
		}
	}

	// age_ms_present assertion
	if exp.AgeMsPresent != nil {
		if *exp.AgeMsPresent && !ageMsSet {
			return false, fmt.Sprintf("age_ms_present=true but age_ms=%d", ageMs)
		}
		if !*exp.AgeMsPresent && ageMsSet {
			return false, fmt.Sprintf("age_ms_present=false but age_ms=%d", ageMs)
		}
	}

	// stale assertion
	if exp.Stale != nil {
		if stale != *exp.Stale {
			return false, fmt.Sprintf("stale: got %v, want %v", stale, *exp.Stale)
		}
	}

	return true, ""
}

// jsonType returns the JSON type name of v, mirroring the fixture data_type field.
func jsonType(v interface{}) string {
	if v == nil {
		return "null"
	}
	switch v.(type) {
	case bool:
		return "bool"
	case float64:
		return "number"
	case string:
		return "string"
	case []interface{}:
		return "array"
	case map[string]interface{}:
		return "object"
	}
	return fmt.Sprintf("unknown(%T)", v)
}

// toText converts a value to a string for data_as_text assertion.
func toText(v interface{}) string {
	if v == nil {
		return ""
	}
	switch x := v.(type) {
	case string:
		return x
	case float64:
		if x == float64(int64(x)) {
			return fmt.Sprintf("%d", int64(x))
		}
		return fmt.Sprintf("%g", x)
	case bool:
		if x {
			return "true"
		}
		return "false"
	}
	return fmt.Sprintf("%v", v)
}

// deepEqual performs structural equality comparable to JSON deep-equal.
// JSON numbers decoded via interface{} are float64 regardless of the fixture's
// integer literal, so we normalise accordingly.
func deepEqual(a, b interface{}) bool {
	// Marshal both to JSON and compare — simple, correct, stdlib-only.
	aj, err := json.Marshal(a)
	if err != nil {
		return false
	}
	bj, err := json.Marshal(b)
	if err != nil {
		return false
	}
	return string(aj) == string(bj)
}
