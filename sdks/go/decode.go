package beachcomber

import "encoding/json"

// ---------------------------------------------------------------------------
// Parse helpers — decode an envelope's already-unwrapped "data" field into
// typed results. See libbeachcomber-ffi/src/lib.rs for the wire shapes
// these mirror.
// ---------------------------------------------------------------------------

// decodeValue decodes the value Get and Eval answer (the envelope's data
// field) as encoding/json decodes into an interface{}: null, a miss, is nil.
func decodeValue(raw json.RawMessage) (interface{}, error) {
	if len(raw) == 0 || string(raw) == "null" {
		return nil, nil
	}
	var v interface{}
	if err := json.Unmarshal(raw, &v); err != nil {
		return nil, parseError("malformed data: " + err.Error())
	}
	return v, nil
}

func parseHelloFromData(raw json.RawMessage) (*HelloInfo, error) {
	var m map[string]interface{}
	if err := json.Unmarshal(raw, &m); err != nil {
		return nil, parseError("hello data is not an object")
	}
	pv, _ := m["protocol_version"].(string)
	dv, _ := m["daemon_version"].(string)
	if pv == "" || dv == "" {
		return nil, parseError("hello response missing versions")
	}
	return &HelloInfo{ProtocolVersion: pv, DaemonVersion: dv}, nil
}

func parseCacheRowsFromData(raw json.RawMessage) ([]CacheRow, error) {
	var arr []map[string]interface{}
	if err := json.Unmarshal(raw, &arr); err != nil {
		return nil, parseError("status data is not an array")
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
				return nil, parseError("malformed introspect data: " + err.Error())
			}
		}
		resp.Other = v
		return resp, nil
	}

	var m map[string]interface{}
	if err := json.Unmarshal(raw, &m); err != nil {
		return nil, parseError("daemon introspect data is not an object")
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
		ri := &ReaperStatus{}
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
			return nil, parseError("malformed watch event data: " + err.Error())
		}
		ev.Data = d
	}
	return ev, nil
}
