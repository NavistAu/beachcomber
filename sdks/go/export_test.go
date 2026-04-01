package beachcomber

// ParseResponseForTest exposes the internal parseResponse function so external
// test packages can construct Result values without a live daemon.
func ParseResponseForTest(raw []byte) (*Result, error) {
	return parseResponse(raw)
}
