require_relative 'test_helper'

class TestOps < Minitest::Test
  def setup
    @server = MockServer.new
    @client = Beachcomber::Client.new(socket_path: @server.path)
  end

  def teardown
    @server.stop
  end

  # --- hello ---

  def test_hello_sends_correct_op
    @server.enqueue('{"ok":true,"data":{"protocol_version":"1","daemon_version":"0.5.0"}}')
    @client.hello
    assert_equal 'hello', @server.requests.last['op']
  end

  def test_hello_returns_hello_info
    @server.enqueue('{"ok":true,"data":{"protocol_version":"1","daemon_version":"0.5.0"}}')
    info = @client.hello
    assert_instance_of Beachcomber::HelloInfo, info
    assert_equal '1',     info.protocol_version
    assert_equal '0.5.0', info.daemon_version
  end

  # The shared client parses hello's response strictly: protocol_version and
  # daemon_version must both be present (libbeachcomber::parse_hello_response),
  # unlike the pre-ABI Ruby client which defaulted missing fields to "".
  def test_hello_missing_data_fields_raises_parse_error
    @server.enqueue('{"ok":true,"data":{}}')
    assert_raises(Beachcomber::ParseError) { @client.hello }
  end

  # --- put ---

  def test_put_sends_correct_op
    @server.enqueue('{"ok":true}')
    @client.put('custom.value', 'hello')
    req = @server.requests.last
    assert_equal 'put',          req['op']
    assert_equal 'custom.value', req['key']
    assert_equal 'hello',        req['data']
  end

  def test_put_nil_sends_null_data
    @server.enqueue('{"ok":true}')
    @client.put('custom.value', nil)
    req = @server.requests.last
    assert req.key?('data')
    assert_nil req['data']
  end

  def test_put_returns_nil
    @server.enqueue('{"ok":true}')
    result = @client.put('custom.value', 42)
    assert_nil result
  end

  # The wire protocol's ttl field is a String (src/protocol.rs); bc_put's
  # `ttl` parameter is a C string, so a non-string Ruby value is coerced via
  # #to_s rather than sent as a JSON number.
  def test_put_with_ttl
    @server.enqueue('{"ok":true}')
    @client.put('custom.value', 'x', ttl: 60)
    req = @server.requests.last
    assert_equal '60', req['ttl']
  end

  def test_put_with_path
    @server.enqueue('{"ok":true}')
    @client.put('custom.value', 'x', path: '/repo')
    req = @server.requests.last
    assert_equal '/repo', req['path']
  end

  def test_put_requires_data
    assert_raises(ArgumentError) { @client.put('custom.value') }
  end

  def test_put_omits_ttl_when_not_given
    @server.enqueue('{"ok":true}')
    @client.put('custom.value', 'x')
    req = @server.requests.last
    refute req.key?('ttl')
  end

  # --- introspect ---

  def test_introspect_sends_correct_op_and_subject
    @server.enqueue('{"ok":true,"data":{"provider_count":5}}')
    @client.introspect(Beachcomber::IntrospectSubject::PROVIDERS)
    req = @server.requests.last
    assert_equal 'introspect', req['op']
    assert_equal 'providers',  req['subject']
  end

  def test_introspect_returns_introspect_response
    @server.enqueue('{"ok":true,"data":{"provider_count":5}}')
    resp = @client.introspect('providers')
    assert_instance_of Beachcomber::IntrospectResponse, resp
    assert_equal 'providers', resp.subject
    assert_nil resp.daemon
    assert_equal({ 'provider_count' => 5 }, resp.other)
  end

  def test_introspect_daemon_subject_returns_daemon_health
    payload = {
      ok: true,
      data: {
        pid: 1234,
        version: '0.5.0',
        uptime_secs: 3600,
        socket_path: '/tmp/sock',
        config_path: '/home/user/.config/beachcomber/config.toml',
        requests_total: 100,
        in_flight: 2,
        active_watchers: 1,
        cache_entries: 42,
        watch_backend: 'native',
        verdicts: [{ level: 'ok', message: 'all good' }],
      }
    }.to_json
    @server.enqueue(payload)
    resp = @client.introspect(Beachcomber::IntrospectSubject::DAEMON)
    assert_instance_of Beachcomber::IntrospectResponse, resp
    assert_equal 'daemon', resp.subject
    assert_nil resp.other

    health = resp.daemon
    assert_instance_of Beachcomber::DaemonHealth, health
    assert_equal 1234,    health.pid
    assert_equal '0.5.0', health.version
    assert_equal 3600,    health.uptime_secs
    assert_equal '/tmp/sock', health.socket_path
    assert_equal 100,     health.requests_total
    assert_equal 2,       health.in_flight
    assert_equal 1,       health.active_watchers
    assert_equal 42,      health.cache_entries
    assert_equal 1,       health.verdicts.size
    assert_instance_of Beachcomber::Verdict, health.verdicts.first
    assert_equal 'ok',       health.verdicts.first.level
    assert_equal 'all good', health.verdicts.first.message
  end

  def test_introspect_with_duration_secs
    @server.enqueue('{"ok":true,"data":null}')
    @client.introspect('cache', duration_secs: 5)
    req = @server.requests.last
    assert_equal 5, req['duration_secs']
  end

  def test_introspect_omits_duration_when_not_given
    @server.enqueue('{"ok":true,"data":null}')
    @client.introspect('cache')
    req = @server.requests.last
    refute req.key?('duration_secs')
  end

  # --- status ---

  def test_status_sends_status_op
    @server.enqueue('{"ok":true,"data":[]}')
    @client.status
    assert_equal 'status', @server.requests.last['op']
  end

  def test_status_returns_array_of_cache_rows
    payload = {
      ok: true,
      data: [
        { provider: 'git', field: 'branch', path: '/repo', source: 'source(head)', value: 'main', age_ms: 100, stale: false },
        { provider: 'git', field: 'dirty',  path: '/repo', source: 'source(dirty)', value: true, age_ms: 200, stale: true },
      ]
    }.to_json
    @server.enqueue(payload)
    rows = @client.status
    assert_equal 2, rows.size
    rows.each { |r| assert_instance_of Beachcomber::CacheRow, r }
    assert_equal 'git',   rows[0].provider
    assert_equal 'branch', rows[0].field
    assert_equal 'main',  rows[0].value
    assert_equal 100,     rows[0].age_ms
    refute rows[0].stale
    assert rows[1].stale
  end

  def test_status_raises_protocol_error_when_data_not_array
    @server.enqueue('{"ok":true,"data":{"not":"an array"}}')
    assert_raises(Beachcomber::ParseError) { @client.status }
  end

  # --- get flags ---

  def test_get_sends_force
    @server.enqueue('{"ok":true,"data":"main","age_ms":0,"stale":false}')
    @client.get('git.branch', force: true)
    req = @server.requests.last
    assert_equal 'get', req['op']
    assert_equal true,  req['force']
    refute req.key?('wait')
  end

  def test_get_sends_wait
    @server.enqueue('{"ok":true,"data":"main","age_ms":0,"stale":false}')
    @client.get('git.branch', wait: true)
    req = @server.requests.last
    assert_equal true, req['wait']
    refute req.key?('force')
  end

  def test_get_omits_flags_by_default
    @server.enqueue('{"ok":true,"data":"main","age_ms":0,"stale":false}')
    @client.get('git.branch')
    req = @server.requests.last
    refute req.key?('force')
    refute req.key?('wait')
  end

  # --- watch ---

  # A watch is a *resolved* watch: it resolves the key once, subscribes to
  # every key that resolution read plus the provider's template tree, and
  # re-resolves on every frame. So several requests reach the server, not
  # one — the same canned answer is queued for each of them, and what these
  # tests pin is the ops on the wire and the value the event carries.
  #
  # `count` is a guess at how many requests the fan-in makes. If it grows
  # past that guess, MockServer's own queue-empty fallback
  # ('{"ok":true}') would otherwise answer the extra request silently, so a
  # wrong guess surfaces as some other assertion failing for an unrelated
  # reason instead of naming the real problem. A queued response past
  # `count` names it instead: the request that reaches it gets a
  # `ServerError` a test can see and understand immediately.
  def enqueue_many(response, count = 8)
    count.times { @server.enqueue(response) }
    @server.enqueue(%({"ok":false,"error":"mock: enqueue_many(#{count}) exhausted — the fan-in made more requests than expected"}))
  end

  def watch_requests
    @server.requests.select { |r| r['op'] == 'watch' }
  end

  def test_watch_subscribes_to_the_key
    enqueue_many('{"ok":true,"data":"main","age_ms":10,"stale":false}')
    stream = @client.watch('git.branch', cwd: '/repo')
    event = stream.next_event
    stream.close

    keys = watch_requests.map { |r| r['key'] }
    assert_equal ['git.branch'], keys.uniq
    assert_instance_of Beachcomber::WatchEvent, event
    assert_equal 'main', event.data
  end

  def test_watch_addresses_the_given_cwd
    enqueue_many('{"ok":true,"data":"main","age_ms":0,"stale":false}')
    stream = @client.watch('git.branch', cwd: '/repo')
    stream.next_event
    stream.close
    refute_empty watch_requests
    assert(watch_requests.all? { |r| r['path'] == '/repo' })
  end

  # A miss is an event carrying nil, not the end of the stream: the daemon's
  # `{"ok":true}` is nothing stored at that address, and a later put can
  # still give the key a value.
  def test_watch_event_on_a_miss_carries_nil_rather_than_ending_the_stream
    enqueue_many('{"ok":true}')
    stream = @client.watch('nothing.here', cwd: '/repo')
    event = stream.next_event
    stream.close
    assert_instance_of Beachcomber::WatchEvent, event
    assert_nil event.data
  end

  def test_watch_stream_is_enumerable
    assert_includes Beachcomber::WatchStream.ancestors, Enumerable
  end

  def test_watch_cancel_unblocks_pending_next
    enqueue_many('{"ok":true,"data":"main","age_ms":0,"stale":false}')
    stream = @client.watch('git.branch', cwd: '/repo')
    stream.next_event
    stream.cancel
    # After cancel, next_event returns nil (outcome: cancelled) rather than
    # blocking indefinitely.
    assert_nil stream.next_event
    stream.close
  end

  # A wait that runs out with no event is the timeout error, not the end of
  # the stream: the stream is still open, and a later change still arrives.
  def test_next_event_that_runs_out_its_timeout_raises_timeout_error
    enqueue_many('{"ok":true,"data":"main","age_ms":0,"stale":false}')
    stream = @client.watch('git.branch', cwd: '/repo')
    stream.next_event
    err = assert_raises(Beachcomber::TimeoutError) { stream.next_event(0) }
    assert_equal 'timeout', err.kind
    stream.close
  end

  def test_watch_without_cwd_watches_the_global_entry
    enqueue_many('{"ok":true,"data":"main","age_ms":0,"stale":false}')
    stream = @client.watch('git.branch')
    stream.next_event
    stream.close
    refute_empty watch_requests
    assert(watch_requests.none? { |r| r.key?('path') }, watch_requests.inspect)
  end

  def test_close_is_idempotent
    enqueue_many('{"ok":true,"data":"main","age_ms":0,"stale":false}')
    stream = @client.watch('git.branch', cwd: '/repo')
    stream.close
    stream.close
  end

  # --- get / eval ---
  # (the daemon holds the template, answered on the `get` for the key; the
  # client renders it, and env.* refs never touch the daemon)

  def test_get_fetches_the_key_at_cwd_and_renders_its_template
    @server.enqueue('{"ok":true,"template":"{{ env.PYVAR | basename }}"}')
    value = @client.get('filters.based', cwd: '/tmp', env: { 'PYVAR' => '/foo/bar/baz' })
    assert_equal 'baz', value
    req = @server.requests.last
    assert_equal 'get',           req['op']
    assert_equal 'filters.based', req['key']
    assert_equal '/tmp',          req['path']
  end

  def test_get_miss_at_cwd_is_nil
    @server.enqueue('{"ok":true}')
    assert_nil @client.get('nope.field', cwd: '/tmp')
  end

  def test_get_plain_field_is_its_cached_value
    @server.enqueue('{"ok":true,"data":"main","age_ms":10,"stale":false}')
    assert_equal 'main', @client.get('git.branch', cwd: '/repo')
  end

  def test_eval_evaluates_raw_template
    result = @client.eval('{{ env.X | truncate(3) }}', cwd: '/tmp', env: { 'X' => 'abcdefgh' })
    assert_equal 'ab…', result
  end

  def test_eval_without_cwd_reads_the_global_entry
    @server.enqueue('{"ok":true,"data":"x","age_ms":0,"stale":false}')
    assert_equal 'x', @client.eval('{{ p.f }}')
    req = @server.requests.last
    assert_equal 'get', req['op']
    refute req.key?('path'), req.inspect
  end

  # --- session additions ---

  def test_session_put
    @server.enqueue('{"ok":true}')
    @client.session do |s|
      result = s.put('mykey', 'myvalue')
      assert_nil result
    end
    req = @server.requests.last
    assert_equal 'put',     req['op']
    assert_equal 'mykey',   req['key']
    assert_equal 'myvalue', req['data']
  end

  def test_session_get_takes_flags_and_env
    @server.enqueue('{"ok":true,"template":"{{ env.WHO }}"}')
    result = nil
    @client.session do |s|
      result = s.get('git.branch', env: { 'WHO' => 'me' }, force: true)
    end
    assert_equal 'me', result
    req = @server.requests.last
    assert_equal true, req['force']
  end

  # --- lifecycle fields on CacheRow ---

  def test_status_row_exposes_lifecycle_fields
    payload = {
      ok: true,
      data: [
        {
          provider: 'git',
          field: 'branch',
          path: '/repo',
          source: 'source(head)',
          value: 'main',
          age_ms: 100,
          stale: false,
          kind: { kind: 'lifecycle', decay: 0, depth: 4, watches_files: true },
          poll_interval_secs: 30,
          poll_count: 3,
          fsevents_reinstate: false,
          next_poll_in_secs: 12,
        }
      ]
    }.to_json
    @server.enqueue(payload)
    rows = @client.status
    git = rows.find { |r| r.provider == 'git' }
    refute_nil git.kind
    assert_equal 'lifecycle', git.kind['kind']
    assert git.poll_interval_secs > 0
    assert git.poll_count > 0
    refute_nil git.fsevents_reinstate
    assert_equal 12, git.next_poll_in_secs
  end

  # --- IntrospectSubject constants ---

  def test_introspect_subject_constants_defined
    assert_equal 'daemon',    Beachcomber::IntrospectSubject::DAEMON
    assert_equal 'providers', Beachcomber::IntrospectSubject::PROVIDERS
    assert_equal 'config',    Beachcomber::IntrospectSubject::CONFIG
    assert_equal 'cache',     Beachcomber::IntrospectSubject::CACHE
    assert_equal 'lifecycle', Beachcomber::IntrospectSubject::LIFECYCLE
    assert_equal 'watches',   Beachcomber::IntrospectSubject::WATCHES
    assert_equal 'timers',    Beachcomber::IntrospectSubject::TIMERS
    assert_equal 'demand',    Beachcomber::IntrospectSubject::DEMAND
    assert_equal 'procs',     Beachcomber::IntrospectSubject::PROCS
  end
end
