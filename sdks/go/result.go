package beachcomber

import "encoding/json"

// Result wraps a successful daemon response. When OK is false the Error field
// contains the daemon-supplied message. Data holds the decoded payload; its
// concrete type is whatever encoding/json unmarshals into (map, slice, scalar).
type Result struct {
	OK    bool
	Data  interface{}
	AgeMs uint64
	Stale bool
	Error string

	// raw preserves the original JSON for callers that need it.
	raw []byte
}

// IsHit reports whether the response carried data (a cache hit).
func (r *Result) IsHit() bool {
	return r.OK && r.Data != nil
}

// IsMiss reports whether the response was successful but contained no data.
func (r *Result) IsMiss() bool {
	return r.OK && r.Data == nil
}

// RawJSON returns the original JSON bytes received from the daemon.
func (r *Result) RawJSON() []byte {
	return r.raw
}

// GetString extracts a string field when Data is a JSON object. If Data is
// itself a string and field is empty, the string value is returned. The second
// return value is false when the field is absent, is not a string, or Data is
// not an object.
func (r *Result) GetString(field string) (string, bool) {
	if r.Data == nil {
		return "", false
	}
	if field == "" {
		s, ok := r.Data.(string)
		return s, ok
	}
	m, ok := r.Data.(map[string]interface{})
	if !ok {
		return "", false
	}
	v, ok := m[field]
	if !ok {
		return "", false
	}
	s, ok := v.(string)
	return s, ok
}

// GetInt extracts an integer field. JSON numbers are decoded as float64 by
// encoding/json so the value is truncated to int64.
func (r *Result) GetInt(field string) (int64, bool) {
	f, ok := r.getFloat(field)
	if !ok {
		return 0, false
	}
	return int64(f), true
}

// GetFloat extracts a numeric field as float64.
func (r *Result) GetFloat(field string) (float64, bool) {
	return r.getFloat(field)
}

// GetBool extracts a boolean field.
func (r *Result) GetBool(field string) (bool, bool) {
	if r.Data == nil {
		return false, false
	}
	if field == "" {
		b, ok := r.Data.(bool)
		return b, ok
	}
	m, ok := r.Data.(map[string]interface{})
	if !ok {
		return false, false
	}
	v, ok := m[field]
	if !ok {
		return false, false
	}
	b, ok := v.(bool)
	return b, ok
}

func (r *Result) getFloat(field string) (float64, bool) {
	if r.Data == nil {
		return 0, false
	}
	if field == "" {
		f, ok := r.Data.(float64)
		return f, ok
	}
	m, ok := r.Data.(map[string]interface{})
	if !ok {
		return 0, false
	}
	v, ok := m[field]
	if !ok {
		return 0, false
	}
	f, ok := v.(float64)
	return f, ok
}

// response is the internal wire type used for JSON unmarshalling.
type response struct {
	OK    bool            `json:"ok"`
	Data  json.RawMessage `json:"data"`
	AgeMs uint64          `json:"age_ms"`
	Stale bool            `json:"stale"`
	Error string          `json:"error"`
}

// ---------------------------------------------------------------------------
// Parse helpers for typed responses
// ---------------------------------------------------------------------------

func parseHelloFromResult(r *Result) (*HelloInfo, error) {
	m, ok := r.Data.(map[string]interface{})
	if !ok {
		return nil, &ProtocolError{msg: "hello data is not an object"}
	}
	pv, _ := m["protocol_version"].(string)
	dv, _ := m["daemon_version"].(string)
	if pv == "" || dv == "" {
		return nil, &ProtocolError{msg: "hello response missing versions"}
	}
	return &HelloInfo{ProtocolVersion: pv, DaemonVersion: dv}, nil
}

func parseCacheRowsFromResult(r *Result) ([]CacheRow, error) {
	arr, ok := r.Data.([]interface{})
	if !ok {
		return nil, &ProtocolError{msg: "status data is not an array"}
	}
	rows := make([]CacheRow, 0, len(arr))
	for _, v := range arr {
		m, ok := v.(map[string]interface{})
		if !ok {
			continue
		}
		row := CacheRow{}
		if s, ok := m["provider"].(string); ok {
			row.Provider = s
		}
		if s, ok := m["field"].(string); ok {
			row.Field = s
		}
		if s, ok := m["path"].(string); ok {
			row.Path = s
		}
		row.Value = m["value"]
		if f, ok := m["age_ms"].(float64); ok {
			row.AgeMs = uint64(f)
		}
		if b, ok := m["stale"].(bool); ok {
			row.Stale = b
		}
		rows = append(rows, row)
	}
	return rows, nil
}

func parseIntrospectFromResult(subject IntrospectSubject, r *Result) (*IntrospectResponse, error) {
	resp := &IntrospectResponse{Subject: subject}
	if subject == SubjectDaemon {
		m, ok := r.Data.(map[string]interface{})
		if !ok {
			return nil, &ProtocolError{msg: "daemon introspect data is not an object"}
		}
		h := &DaemonHealth{}
		if f, ok := m["pid"].(float64); ok {
			h.PID = int64(f)
		}
		if s, ok := m["version"].(string); ok {
			h.Version = s
		}
		if f, ok := m["uptime_secs"].(float64); ok {
			h.UptimeSecs = uint64(f)
		}
		if s, ok := m["socket_path"].(string); ok {
			h.SocketPath = s
		}
		if s, ok := m["config_path"].(string); ok {
			h.ConfigPath = s
		}
		if f, ok := m["requests_total"].(float64); ok {
			h.RequestsTotal = uint64(f)
		}
		if f, ok := m["in_flight"].(float64); ok {
			h.InFlight = uint64(f)
		}
		if f, ok := m["active_watchers"].(float64); ok {
			h.ActiveWatchers = uint64(f)
		}
		if f, ok := m["cache_entries"].(float64); ok {
			h.CacheEntries = uint64(f)
		}
		if arr, ok := m["verdicts"].([]interface{}); ok {
			for _, v := range arr {
				vm, ok := v.(map[string]interface{})
				if !ok {
					continue
				}
				lv, _ := vm["level"].(string)
				mv, _ := vm["message"].(string)
				h.Verdicts = append(h.Verdicts, Verdict{Level: lv, Message: mv})
			}
		}
		resp.Daemon = h
		return resp, nil
	}
	resp.Other = r.Data
	return resp, nil
}

func parseResponse(raw []byte) (*Result, error) {
	var resp response
	if err := json.Unmarshal(raw, &resp); err != nil {
		return nil, &ProtocolError{msg: "malformed JSON: " + err.Error()}
	}

	result := &Result{
		OK:    resp.OK,
		AgeMs: resp.AgeMs,
		Stale: resp.Stale,
		Error: resp.Error,
		raw:   raw,
	}

	if len(resp.Data) > 0 && string(resp.Data) != "null" {
		var data interface{}
		if err := json.Unmarshal(resp.Data, &data); err != nil {
			return nil, &ProtocolError{msg: "malformed data field: " + err.Error()}
		}
		result.Data = data
	}

	if !result.OK {
		return nil, &ServerError{Message: result.Error}
	}

	return result, nil
}
