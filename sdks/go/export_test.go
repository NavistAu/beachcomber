package beachcomber

import "encoding/json"

// DecodeValueForTest exposes the decoder Get and Eval share, so external
// test packages can check how the value bc_get's data field carries decodes
// without a live daemon or library.
func DecodeValueForTest(raw []byte) (interface{}, error) {
	return decodeValue(json.RawMessage(raw))
}

// OptionsJSONForTest exposes the options_json NewClient hands bc_client_new,
// so tests can check an Option reaches the library.
func OptionsJSONForTest(options ...Option) string {
	return optionsJSON(options...)
}
