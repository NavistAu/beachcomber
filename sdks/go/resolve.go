package beachcomber

import "runtime"

// Resolve evaluates a virtual field ("provider.field") or a path expression
// (a bare provider name) client-side — the same resolution comb get's
// resolution layer performs, exposed here via bc_resolve.
//
// cwd is required: path expressions select a cache coordinate over it, so
// this library never falls back to the process's own working directory on
// the caller's behalf. env and overrides are both optional; pass nil for
// "not supplied" — a nil env makes every env.* reference resolve to "",
// and a nil overrides uses the built-in default expressions. overrides maps
// a field key ("provider.field") or a bare provider name to the expression
// string that should override the built-in default for it, matching the
// conformance fixture format's "virtual" block.
func (c *Client) Resolve(key, cwd string, env, overrides map[string]string) (*Result, error) {
	if c.loadErr != nil {
		return nil, c.loadErr
	}
	keyBuf, cwdBuf := cBytes(key), cBytes(cwd)
	envBuf, overridesBuf := jsonMapBytes(env), jsonMapBytes(overrides)
	data, err := c.lib.call(c.lib.bcResolve, c.handle, ptrOf(keyBuf), ptrOf(cwdBuf), ptrOf(envBuf), ptrOf(overridesBuf))
	runtime.KeepAlive(keyBuf)
	runtime.KeepAlive(cwdBuf)
	runtime.KeepAlive(envBuf)
	runtime.KeepAlive(overridesBuf)
	if err != nil {
		return nil, err
	}
	return resultFromScalarData(data)
}

// Eval evaluates an arbitrary expression string — the same evaluator
// Resolve uses for a declared virtual field, but for a raw expression that
// need not be registered anywhere. See Resolve for cwd/env/overrides
// semantics, which this shares.
func (c *Client) Eval(templateStr, cwd string, env, overrides map[string]string) (*Result, error) {
	if c.loadErr != nil {
		return nil, c.loadErr
	}
	tplBuf, cwdBuf := cBytes(templateStr), cBytes(cwd)
	envBuf, overridesBuf := jsonMapBytes(env), jsonMapBytes(overrides)
	data, err := c.lib.call(c.lib.bcEval, c.handle, ptrOf(tplBuf), ptrOf(cwdBuf), ptrOf(envBuf), ptrOf(overridesBuf))
	runtime.KeepAlive(tplBuf)
	runtime.KeepAlive(cwdBuf)
	runtime.KeepAlive(envBuf)
	runtime.KeepAlive(overridesBuf)
	if err != nil {
		return nil, err
	}
	return resultFromScalarData(data)
}
