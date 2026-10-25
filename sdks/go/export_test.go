package beachcomber

import "encoding/json"

// ParseResultForTest exposes the internal decoder Get and Eval share, so
// external test packages can construct Result values from the value bc_get's
// data field carries without a live daemon or library.
func ParseResultForTest(raw []byte) (*Result, error) {
	return resultFromScalarData(json.RawMessage(raw))
}

// OptionsJSONForTest exposes the options_json a client constructor hands
// bc_client_new, so tests can check an Option reaches the library.
func OptionsJSONForTest(socketPath string, options ...Option) string {
	return optionsJSON(socketPath, options...)
}
