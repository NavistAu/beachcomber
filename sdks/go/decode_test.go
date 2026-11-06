package beachcomber_test

import (
	"errors"
	"reflect"
	"testing"

	beachcomber "github.com/NavistAu/beachcomber/sdks/go"
)

// Get and Eval answer the value itself, as encoding/json decodes it; null,
// a miss, is nil.
func TestDecodeValue_IsTheBareValue(t *testing.T) {
	for raw, want := range map[string]interface{}{
		`null`:             nil,
		``:                 nil,
		`"main"`:           "main",
		`42`:               float64(42),
		`true`:             true,
		`{"branch":"x"}`:   map[string]interface{}{"branch": "x"},
		`["a",{"b":null}]`: []interface{}{"a", map[string]interface{}{"b": nil}},
	} {
		got, err := beachcomber.DecodeValueForTest([]byte(raw))
		if err != nil {
			t.Fatalf("decode %q: %v", raw, err)
		}
		if !reflect.DeepEqual(got, want) {
			t.Errorf("decode %q = %#v, want %#v", raw, got, want)
		}
	}
}

func TestDecodeValue_MalformedIsAParseError(t *testing.T) {
	_, err := beachcomber.DecodeValueForTest([]byte(`{"open"`))
	if !errors.Is(err, beachcomber.ErrParse) {
		t.Errorf("decode of malformed JSON = %v, want a parse_error", err)
	}
}
