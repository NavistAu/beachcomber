package beachcomber_test

import (
	"encoding/json"
	"testing"

	beachcomber "github.com/NavistAu/beachcomber/sdks/go"
)

// buildResult constructs a *Result by round-tripping through the
// package-internal decoder (exposed for tests via export_test.go), from the
// value bc_get's and bc_eval's envelope "data" field carries.
func buildResult(t *testing.T, wireJSON string) *beachcomber.Result {
	t.Helper()
	r, err := beachcomber.ParseResultForTest([]byte(wireJSON))
	if err != nil {
		t.Fatalf("buildResult: %v (json: %s)", err, wireJSON)
	}
	return r
}

// ---------------------------------------------------------------------------
// IsHit / IsMiss: a value, or nothing
// ---------------------------------------------------------------------------

func TestResult_IsHit_WithData(t *testing.T) {
	r := buildResult(t, `"main"`)
	if !r.IsHit() {
		t.Error("expected IsHit() = true")
	}
	if r.IsMiss() {
		t.Error("expected IsMiss() = false")
	}
}

func TestResult_IsMiss_Null(t *testing.T) {
	r := buildResult(t, `null`)
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
	r := buildResult(t, `"main"`)
	s, ok := r.GetString("")
	if !ok || s != "main" {
		t.Errorf("GetString(\"\") = (%q, %v), want (\"main\", true)", s, ok)
	}
}

func TestResult_GetString_ObjectField(t *testing.T) {
	r := buildResult(t, `{"branch":"main","dirty":true}`)
	s, ok := r.GetString("branch")
	if !ok || s != "main" {
		t.Errorf("GetString(\"branch\") = (%q, %v), want (\"main\", true)", s, ok)
	}
}

func TestResult_GetString_MissingField(t *testing.T) {
	r := buildResult(t, `{"branch":"main"}`)
	_, ok := r.GetString("nonexistent")
	if ok {
		t.Error("GetString(\"nonexistent\") should return false")
	}
}

func TestResult_GetString_WrongType(t *testing.T) {
	r := buildResult(t, `{"count":42}`)
	_, ok := r.GetString("count")
	if ok {
		t.Error("GetString on a numeric field should return false")
	}
}

func TestResult_GetString_NilData(t *testing.T) {
	r := buildResult(t, `null`)
	_, ok := r.GetString("x")
	if ok {
		t.Error("GetString on nil data should return false")
	}
}

// ---------------------------------------------------------------------------
// GetInt / GetFloat / GetBool
// ---------------------------------------------------------------------------

func TestResult_GetInt_ScalarHit(t *testing.T) {
	r := buildResult(t, `42`)
	n, ok := r.GetInt("")
	if !ok || n != 42 {
		t.Errorf("GetInt(\"\") = (%d, %v), want (42, true)", n, ok)
	}
}

func TestResult_GetInt_ObjectField(t *testing.T) {
	r := buildResult(t, `{"staged":3,"branch":"main"}`)
	n, ok := r.GetInt("staged")
	if !ok || n != 3 {
		t.Errorf("GetInt(\"staged\") = (%d, %v), want (3, true)", n, ok)
	}
}

func TestResult_GetFloat_ScalarHit(t *testing.T) {
	r := buildResult(t, `3.14`)
	f, ok := r.GetFloat("")
	if !ok || f != 3.14 {
		t.Errorf("GetFloat(\"\") = (%f, %v), want (3.14, true)", f, ok)
	}
}

func TestResult_GetBool_ScalarTrue(t *testing.T) {
	r := buildResult(t, `true`)
	b, ok := r.GetBool("")
	if !ok || !b {
		t.Errorf("GetBool(\"\") = (%v, %v), want (true, true)", b, ok)
	}
}

func TestResult_GetBool_False(t *testing.T) {
	r := buildResult(t, `{"detached":false}`)
	b, ok := r.GetBool("detached")
	if !ok || b {
		t.Errorf("GetBool(\"detached\") = (%v, %v), want (false, true)", b, ok)
	}
}

// ---------------------------------------------------------------------------
// RawJSON
// ---------------------------------------------------------------------------

func TestResult_RawJSON(t *testing.T) {
	r := buildResult(t, `{"branch":"main"}`)
	var m map[string]interface{}
	if err := json.Unmarshal(r.RawJSON(), &m); err != nil {
		t.Fatalf("RawJSON is not valid JSON: %v", err)
	}
	if m["branch"] != "main" {
		t.Errorf("RawJSON = %s, want the value as decoded", r.RawJSON())
	}
}
