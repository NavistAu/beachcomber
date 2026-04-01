package beachcomber_test

import (
	"encoding/json"
	"testing"

	beachcomber "github.com/NavistAu/beachcomber/sdks/go"
)

// buildResult is a test helper that constructs a *Result by round-tripping
// through the public JSON wire format so we don't have to touch unexported
// fields.
func buildResult(t *testing.T, wireJSON string) *beachcomber.Result {
	t.Helper()
	r, err := resultFromWire(wireJSON)
	if err != nil {
		t.Fatalf("buildResult: %v (json: %s)", err, wireJSON)
	}
	return r
}

// resultFromWire calls the package-internal parse path via NewClientWithPath +
// a mini mock server. That is heavy; instead we expose a thin test helper
// through an unexported function. Since result_test.go is in package
// beachcomber_test (external test package) we use a public constructor exposed
// only in export_test.go.
func resultFromWire(wireJSON string) (*beachcomber.Result, error) {
	return beachcomber.ParseResponseForTest([]byte(wireJSON))
}

// ---------------------------------------------------------------------------
// IsHit / IsMiss
// ---------------------------------------------------------------------------

func TestResult_IsHit_WithData(t *testing.T) {
	r := buildResult(t, `{"ok":true,"data":"main","age_ms":10,"stale":false}`)
	if !r.IsHit() {
		t.Error("expected IsHit() = true")
	}
	if r.IsMiss() {
		t.Error("expected IsMiss() = false")
	}
}

func TestResult_IsMiss_NoData(t *testing.T) {
	r := buildResult(t, `{"ok":true}`)
	if r.IsHit() {
		t.Error("expected IsHit() = false")
	}
	if !r.IsMiss() {
		t.Error("expected IsMiss() = true")
	}
}

func TestResult_IsMiss_NullData(t *testing.T) {
	r := buildResult(t, `{"ok":true,"data":null}`)
	if r.IsHit() {
		t.Error("expected IsHit() = false for null data")
	}
	if !r.IsMiss() {
		t.Error("expected IsMiss() = true for null data")
	}
}

// ---------------------------------------------------------------------------
// GetString
// ---------------------------------------------------------------------------

func TestResult_GetString_ScalarHit(t *testing.T) {
	r := buildResult(t, `{"ok":true,"data":"main"}`)
	s, ok := r.GetString("")
	if !ok || s != "main" {
		t.Errorf("GetString(\"\") = (%q, %v), want (\"main\", true)", s, ok)
	}
}

func TestResult_GetString_ObjectField(t *testing.T) {
	r := buildResult(t, `{"ok":true,"data":{"branch":"main","dirty":true}}`)
	s, ok := r.GetString("branch")
	if !ok || s != "main" {
		t.Errorf("GetString(\"branch\") = (%q, %v), want (\"main\", true)", s, ok)
	}
}

func TestResult_GetString_MissingField(t *testing.T) {
	r := buildResult(t, `{"ok":true,"data":{"branch":"main"}}`)
	_, ok := r.GetString("nonexistent")
	if ok {
		t.Error("GetString(\"nonexistent\") should return false")
	}
}

func TestResult_GetString_WrongType(t *testing.T) {
	r := buildResult(t, `{"ok":true,"data":{"count":42}}`)
	_, ok := r.GetString("count")
	if ok {
		t.Error("GetString on a numeric field should return false")
	}
}

func TestResult_GetString_NilData(t *testing.T) {
	r := buildResult(t, `{"ok":true}`)
	_, ok := r.GetString("x")
	if ok {
		t.Error("GetString on nil data should return false")
	}
}

// ---------------------------------------------------------------------------
// GetInt
// ---------------------------------------------------------------------------

func TestResult_GetInt_ScalarHit(t *testing.T) {
	r := buildResult(t, `{"ok":true,"data":42}`)
	n, ok := r.GetInt("")
	if !ok || n != 42 {
		t.Errorf("GetInt(\"\") = (%d, %v), want (42, true)", n, ok)
	}
}

func TestResult_GetInt_ObjectField(t *testing.T) {
	r := buildResult(t, `{"ok":true,"data":{"staged":3,"branch":"main"}}`)
	n, ok := r.GetInt("staged")
	if !ok || n != 3 {
		t.Errorf("GetInt(\"staged\") = (%d, %v), want (3, true)", n, ok)
	}
}

func TestResult_GetInt_WrongType(t *testing.T) {
	r := buildResult(t, `{"ok":true,"data":{"branch":"main"}}`)
	_, ok := r.GetInt("branch")
	if ok {
		t.Error("GetInt on a string field should return false")
	}
}

// ---------------------------------------------------------------------------
// GetFloat
// ---------------------------------------------------------------------------

func TestResult_GetFloat_ScalarHit(t *testing.T) {
	r := buildResult(t, `{"ok":true,"data":3.14}`)
	f, ok := r.GetFloat("")
	if !ok || f != 3.14 {
		t.Errorf("GetFloat(\"\") = (%f, %v), want (3.14, true)", f, ok)
	}
}

func TestResult_GetFloat_ObjectField(t *testing.T) {
	r := buildResult(t, `{"ok":true,"data":{"score":1.5}}`)
	f, ok := r.GetFloat("score")
	if !ok || f != 1.5 {
		t.Errorf("GetFloat(\"score\") = (%f, %v), want (1.5, true)", f, ok)
	}
}

// ---------------------------------------------------------------------------
// GetBool
// ---------------------------------------------------------------------------

func TestResult_GetBool_ScalarTrue(t *testing.T) {
	r := buildResult(t, `{"ok":true,"data":true}`)
	b, ok := r.GetBool("")
	if !ok || !b {
		t.Errorf("GetBool(\"\") = (%v, %v), want (true, true)", b, ok)
	}
}

func TestResult_GetBool_ObjectField(t *testing.T) {
	r := buildResult(t, `{"ok":true,"data":{"dirty":true,"branch":"main"}}`)
	b, ok := r.GetBool("dirty")
	if !ok || !b {
		t.Errorf("GetBool(\"dirty\") = (%v, %v), want (true, true)", b, ok)
	}
}

func TestResult_GetBool_False(t *testing.T) {
	r := buildResult(t, `{"ok":true,"data":{"detached":false}}`)
	b, ok := r.GetBool("detached")
	if !ok || b {
		t.Errorf("GetBool(\"detached\") = (%v, %v), want (false, true)", b, ok)
	}
}

// ---------------------------------------------------------------------------
// RawJSON
// ---------------------------------------------------------------------------

func TestResult_RawJSON(t *testing.T) {
	wire := `{"ok":true,"data":"main","age_ms":10,"stale":false}`
	r := buildResult(t, wire)
	raw := r.RawJSON()
	// The raw bytes should be valid JSON containing the same fields.
	var m map[string]interface{}
	if err := json.Unmarshal(raw, &m); err != nil {
		t.Fatalf("RawJSON is not valid JSON: %v", err)
	}
	if m["ok"] != true {
		t.Error("RawJSON missing ok field")
	}
}

// ---------------------------------------------------------------------------
// AgeMs / Stale propagation
// ---------------------------------------------------------------------------

func TestResult_AgeAndStale(t *testing.T) {
	r := buildResult(t, `{"ok":true,"data":"v","age_ms":9999,"stale":true}`)
	if r.AgeMs != 9999 {
		t.Errorf("AgeMs = %d, want 9999", r.AgeMs)
	}
	if !r.Stale {
		t.Error("expected Stale = true")
	}
}
