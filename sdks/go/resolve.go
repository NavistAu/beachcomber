package beachcomber

import "runtime"

// Resolve resolves key to the same value comb get returns for this key, cwd
// and env, via bc_resolve. The daemon holds the templates
// ("<provider>:template", seeded from configuration or Put); this client
// renders them. A plain field is its cached value, a templated one is
// rendered, and a bare provider name is the whole merged namespace. A miss
// is a Result with nil data.
//
// cwd is required: every fetch the resolution makes addresses it, and this
// library never falls back to the process's own working directory on the
// caller's behalf. env is the caller's environment for env.* references;
// pass nil for "not supplied", which makes every env.* reference resolve to
// "".
func (c *Client) Resolve(key, cwd string, env map[string]string) (*Result, error) {
	if c.loadErr != nil {
		return nil, c.loadErr
	}
	keyBuf, cwdBuf, envBuf := cBytes(key), cBytes(cwd), jsonMapBytes(env)
	data, err := c.lib.call(c.lib.bcResolve, c.handle, ptrOf(keyBuf), ptrOf(cwdBuf), ptrOf(envBuf))
	runtime.KeepAlive(keyBuf)
	runtime.KeepAlive(cwdBuf)
	runtime.KeepAlive(envBuf)
	if err != nil {
		return nil, err
	}
	return resultFromScalarData(data)
}

// Eval renders a template that is stored nowhere — the same value comb eval
// prints. Every ref resolves exactly as it would inside a stored template,
// so Eval("{{ p.f }}") and Resolve("p.f") agree. See Resolve for cwd/env
// semantics, which this shares.
//
// templateStr accepts a single "{{ expr }}" tag, or literal text/several
// tags — the former keeps the expression's natural type, the latter is
// always a string (a source with no tags at all is its literal text).
func (c *Client) Eval(templateStr, cwd string, env map[string]string) (*Result, error) {
	if c.loadErr != nil {
		return nil, c.loadErr
	}
	tplBuf, cwdBuf, envBuf := cBytes(templateStr), cBytes(cwd), jsonMapBytes(env)
	data, err := c.lib.call(c.lib.bcEval, c.handle, ptrOf(tplBuf), ptrOf(cwdBuf), ptrOf(envBuf))
	runtime.KeepAlive(tplBuf)
	runtime.KeepAlive(cwdBuf)
	runtime.KeepAlive(envBuf)
	if err != nil {
		return nil, err
	}
	return resultFromScalarData(data)
}
