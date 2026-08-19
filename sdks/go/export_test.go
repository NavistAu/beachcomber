package beachcomber

import "encoding/json"

// ParseGetResultForTest exposes the internal get-result decoder so external
// test packages can construct Result values from the shape bc_get's data
// field carries ({"data":...,"age_ms":...,"stale":...}) without a live
// daemon or library.
func ParseGetResultForTest(raw []byte) (*Result, error) {
	return resultFromGetData(json.RawMessage(raw))
}

// ParseScalarResultForTest exposes the internal resolve/eval decoder for the
// same reason — a bare JSON value with no age_ms/stale wrapper.
func ParseScalarResultForTest(raw []byte) (*Result, error) {
	return resultFromScalarData(json.RawMessage(raw))
}
