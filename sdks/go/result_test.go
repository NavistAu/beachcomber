package beachcomber_test

import (
	"encoding/json"
	"testing"

	beachcomber "github.com/NavistAu/beachcomber/sdks/go"
)

// buildResult constructs a *Result by round-tripping through the
// package-internal get-result decoder (exposed for tests via
// export_test.go), the same shape bc_get's envelope "data" field carries.
func buildResult(t *testing.T, wireJSON string) *beachcomber.Result {
	t.Helper()
	r, err := beachcomber.ParseGetResultForTest([]byte(wireJSON))
	if err != nil {
		t.Fatalf("buildResult: %v (json: %s)", err, wireJSON)
	}
	return r
}

// ---------------------------------------------------------------------------
// IsHit / IsMiss
// ---------------------------------------------------------------------------

func TestResult_IsHit_WithData(t *testing.T) {
	r := buildResult(t, `{"data":"main","age_ms":10,"stale":false}`)
	if !r.IsHit() {
		t.Error("expected IsHit() = true")
	}
	if r.IsMiss() {
		t.Error("expected IsMiss() = false")
	}
}

func TestResult_IsMiss_NoData(t *testing.T) {
	r := buildResult(t, `{"data":null,"age_ms":null,"stale":null}`)
	if r.IsHit() {
		t.Error("expected IsHit() = false")
	}
	if !r.IsMiss() {
		t.Error("expected IsMiss() = true")
	}
}

// ---------------------------------------------------------------------------
// GetString
// ---------------------------------------------------------------------------

func TestResult_GetString_ScalarHit(t *testing.T) {
	r := buildResult(t, `{"data":"main","age_ms":1,"stale":false}`)
	s, ok := r.GetString("")
	if !ok || s != "main" {
		t.Errorf("GetString(\"\") = (%q, %v), want (\"main\", true)", s, ok)
	}
}

func TestResult_GetString_ObjectField(t *testing.T) {
	r := buildResult(t, `{"data":{"branch":"main","dirty":true},"age_ms":1,"stale":false}`)
	s, ok := r.GetString("branch")
	if !ok || s != "main" {
		t.Errorf("GetString(\"branch\") = (%q, %v), want (\"main\", true)", s, ok)
	}
}

func TestResult_GetString_MissingField(t *testing.T) {
	r := buildResult(t, `{"data":{"branch":"main"},"age_ms":1,"stale":false}`)
	_, ok := r.GetString("nonexistent")
	if ok {
		t.Error("GetString(\"nonexistent\") should return false")
	}
}

func TestResult_GetString_WrongType(t *testing.T) {
	r := buildResult(t, `{"data":{"count":42},"age_ms":1,"stale":false}`)
	_, ok := r.GetString("count")
	if ok {
		t.Error("GetString on a numeric field should return false")
	}
}

func TestResult_GetString_NilData(t *testing.T) {
	r := buildResult(t, `{"data":null,"age_ms":null,"stale":null}`)
	_, ok := r.GetString("x")
	if ok {
		t.Error("GetString on nil data should return false")
	}
}

// ---------------------------------------------------------------------------
// GetInt / GetFloat / GetBool
// ---------------------------------------------------------------------------

func TestResult_GetInt_ScalarHit(t *testing.T) {
	r := buildResult(t, `{"data":42,"age_ms":1,"stale":false}`)
	n, ok := r.GetInt("")
	if !ok || n != 42 {
		t.Errorf("GetInt(\"\") = (%d, %v), want (42, true)", n, ok)
	}
}

func TestResult_GetInt_ObjectField(t *testing.T) {
	r := buildResult(t, `{"data":{"staged":3,"branch":"main"},"age_ms":1,"stale":false}`)
	n, ok := r.GetInt("staged")
	if !ok || n != 3 {
		t.Errorf("GetInt(\"staged\") = (%d, %v), want (3, true)", n, ok)
	}
}

func TestResult_GetFloat_ScalarHit(t *testing.T) {
	r := buildResult(t, `{"data":3.14,"age_ms":1,"stale":false}`)
	f, ok := r.GetFloat("")
	if !ok || f != 3.14 {
		t.Errorf("GetFloat(\"\") = (%f, %v), want (3.14, true)", f, ok)
	}
}

func TestResult_GetBool_ScalarTrue(t *testing.T) {
	r := buildResult(t, `{"data":true,"age_ms":1,"stale":false}`)
	b, ok := r.GetBool("")
	if !ok || !b {
		t.Errorf("GetBool(\"\") = (%v, %v), want (true, true)", b, ok)
	}
}

func TestResult_GetBool_False(t *testing.T) {
	r := buildResult(t, `{"data":{"detached":false},"age_ms":1,"stale":false}`)
	b, ok := r.GetBool("detached")
	if !ok || b {
		t.Errorf("GetBool(\"detached\") = (%v, %v), want (false, true)", b, ok)
	}
}

// ---------------------------------------------------------------------------
// RawJSON / AgeMs / Stale
// ---------------------------------------------------------------------------

func TestResult_RawJSON(t *testing.T) {
	wire := `{"data":"main","age_ms":10,"stale":false}`
	r := buildResult(t, wire)
	raw := r.RawJSON()
	var m map[string]interface{}
	if err := json.Unmarshal(raw, &m); err != nil {
		t.Fatalf("RawJSON is not valid JSON: %v", err)
	}
	if m["data"] != "main" {
		t.Error("RawJSON missing data field")
	}
}

func TestResult_AgeAndStale(t *testing.T) {
	r := buildResult(t, `{"data":"v","age_ms":9999,"stale":true}`)
	if r.AgeMs != 9999 {
		t.Errorf("AgeMs = %d, want 9999", r.AgeMs)
	}
	if !r.Stale {
		t.Error("expected Stale = true")
	}
}

// ---------------------------------------------------------------------------
// Scalar decoder (Resolve/Eval shape — no age_ms/stale wrapper)
// ---------------------------------------------------------------------------

func TestScalarResult_String(t *testing.T) {
	r, err := beachcomber.ParseScalarResultForTest([]byte(`"baz"`))
	if err != nil {
		t.Fatalf("ParseScalarResultForTest: %v", err)
	}
	s, ok := r.GetString("")
	if !ok || s != "baz" {
		t.Errorf("GetString(\"\") = (%q, %v), want (\"baz\", true)", s, ok)
	}
}

func TestScalarResult_Null(t *testing.T) {
	r, err := beachcomber.ParseScalarResultForTest([]byte(`null`))
	if err != nil {
		t.Fatalf("ParseScalarResultForTest: %v", err)
	}
	if !r.IsMiss() {
		t.Error("expected IsMiss() = true for null")
	}
}
