package beachcomber

import "encoding/json"

// Result wraps a successful read: Data is the value Get or Eval answered,
// decoded into whatever encoding/json unmarshals it to (map, slice, scalar),
// or nil on a miss. Construction always succeeds — an ok:false envelope
// becomes a *ServerError instead of a Result, so every Result you hold
// represents success.
type Result struct {
	OK   bool
	Data interface{}

	// raw preserves the JSON value actually parsed — the envelope's data
	// field, with "ok" already stripped by the binding — for callers that
	// need it.
	raw []byte
}

// IsHit reports whether the read answered a value.
func (r *Result) IsHit() bool {
	return r.OK && r.Data != nil
}

// IsMiss reports whether nothing answers at the address: the read
// succeeded and its value is absent.
func (r *Result) IsMiss() bool {
	return r.OK && r.Data == nil
}

// RawJSON returns the JSON payload backing this Result (see the raw field's
// doc comment for exactly what that covers).
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

// ---------------------------------------------------------------------------
// Parse helpers — decode an envelope's already-unwrapped "data" field into
// typed results. See libbeachcomber-ffi/src/lib.rs for the wire shapes
// these mirror.
// ---------------------------------------------------------------------------

// resultFromScalarData decodes the value Get and Eval answer (the
// envelope's data field, null on a miss) into a Result.
func resultFromScalarData(raw json.RawMessage) (*Result, error) {
	r := &Result{OK: true, raw: raw}
	if len(raw) > 0 && string(raw) != "null" {
		var d interface{}
		if err := json.Unmarshal(raw, &d); err != nil {
			return nil, &ProtocolError{msg: "malformed data: " + err.Error()}
		}
		r.Data = d
	}
	return r, nil
}

func parseHelloFromData(raw json.RawMessage) (*HelloInfo, error) {
	var m map[string]interface{}
	if err := json.Unmarshal(raw, &m); err != nil {
		return nil, &ProtocolError{msg: "hello data is not an object"}
	}
	pv, _ := m["protocol_version"].(string)
	dv, _ := m["daemon_version"].(string)
	if pv == "" || dv == "" {
		return nil, &ProtocolError{msg: "hello response missing versions"}
	}
	return &HelloInfo{ProtocolVersion: pv, DaemonVersion: dv}, nil
}

func parseCacheRowsFromData(raw json.RawMessage) ([]CacheRow, error) {
	var arr []map[string]interface{}
	if err := json.Unmarshal(raw, &arr); err != nil {
		return nil, &ProtocolError{msg: "status data is not an array"}
	}
	rows := make([]CacheRow, 0, len(arr))
	for _, m := range arr {
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
		if k, ok := m["kind"].(map[string]interface{}); ok {
			row.Kind = k
		}
		if p, ok := m["poll_interval_secs"].(float64); ok {
			pInt := uint64(p)
			row.PollIntervalSecs = &pInt
		}
		if p, ok := m["poll_count"].(float64); ok {
			pInt := uint32(p)
			row.PollCount = &pInt
		}
		if b, ok := m["fsevents_reinstate"].(bool); ok {
			row.FseventsReinstate = &b
		}
		if p, ok := m["polls_elapsed"].(float64); ok {
			pInt := uint64(p)
			row.PollsElapsed = &pInt
		}
		if p, ok := m["next_poll_in_secs"].(float64); ok {
			pInt := uint64(p)
			row.NextPollInSecs = &pInt
		}
		if f, ok := m["failure"].(map[string]interface{}); ok {
			row.Failure = f
		}
		if s, ok := m["source"].(string); ok {
			row.Source = s
		}
		rows = append(rows, row)
	}
	return rows, nil
}

func parseIntrospectFromData(subject IntrospectSubject, raw json.RawMessage) (*IntrospectResponse, error) {
	resp := &IntrospectResponse{Subject: subject}
	if subject != SubjectDaemon {
		var v interface{}
		if len(raw) > 0 && string(raw) != "null" {
			if err := json.Unmarshal(raw, &v); err != nil {
				return nil, &ProtocolError{msg: "malformed introspect data: " + err.Error()}
			}
		}
		resp.Other = v
		return resp, nil
	}

	var m map[string]interface{}
	if err := json.Unmarshal(raw, &m); err != nil {
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
	if s, ok := m["watch_backend"].(string); ok {
		h.WatchBackend = s
	}
	if rm, ok := m["reaper"].(map[string]interface{}); ok {
		ri := &ReaperInfo{}
		if b, ok := rm["armed"].(bool); ok {
			ri.Armed = b
		}
		if s, ok := rm["visibility"].(string); ok {
			ri.Visibility = s
		}
		if f, ok := rm["sweeps"].(float64); ok {
			ri.Sweeps = uint64(f)
		}
		if f, ok := rm["reaped"].(float64); ok {
			ri.Reaped = uint64(f)
		}
		if f, ok := rm["kill_denied"].(float64); ok {
			ri.KillDenied = uint64(f)
		}
		h.Reaper = ri
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

// watchEventFromData reads bc_watch_next's event payload, which is the
// resolved value itself (null for a miss) — see WatchOutcome::Event in
// libbeachcomber-ffi/src/envelope.rs.
func watchEventFromData(raw json.RawMessage) (*WatchEvent, error) {
	ev := &WatchEvent{}
	if len(raw) > 0 && string(raw) != "null" {
		var d interface{}
		if err := json.Unmarshal(raw, &d); err != nil {
			return nil, &ProtocolError{msg: "malformed watch event data: " + err.Error()}
		}
		ev.Data = d
	}
	return ev, nil
}
