require_relative 'test_helper'
require 'stringio'

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

  def test_hello_tolerates_missing_data_fields
    @server.enqueue('{"ok":true,"data":{}}')
    info = @client.hello
    assert_equal '', info.protocol_version
    assert_equal '', info.daemon_version
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

  def test_put_returns_nil
    @server.enqueue('{"ok":true}')
    result = @client.put('custom.value', 42)
    assert_nil result
  end

  def test_put_with_ttl
    @server.enqueue('{"ok":true}')
    @client.put('custom.value', 'x', ttl: 60)
    req = @server.requests.last
    assert_equal 60, req['ttl']
  end

  def test_put_with_path
    @server.enqueue('{"ok":true}')
    @client.put('custom.value', 'x', path: '/repo')
    req = @server.requests.last
    assert_equal '/repo', req['path']
  end

  def test_put_omits_data_when_nil
    @server.enqueue('{"ok":true}')
    @client.put('custom.value')
    req = @server.requests.last
    refute req.key?('data')
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
        { provider: 'git', field: 'branch', path: '/repo', value: 'main', age_ms: 100, stale: false },
        { provider: 'git', field: 'dirty',  path: '/repo', value: true,   age_ms: 200, stale: true  },
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
    assert_raises(Beachcomber::ProtocolError) { @client.status }
  end

  # --- get_with_flags ---

  def test_get_with_flags_sends_force
    @server.enqueue('{"ok":true,"data":"main","age_ms":0,"stale":false}')
    @client.get_with_flags('git.branch', force: true)
    req = @server.requests.last
    assert_equal 'get', req['op']
    assert_equal true,  req['force']
    refute req.key?('wait')
  end

  def test_get_with_flags_sends_wait
    @server.enqueue('{"ok":true,"data":"main","age_ms":0,"stale":false}')
    @client.get_with_flags('git.branch', wait: true)
    req = @server.requests.last
    assert_equal true, req['wait']
    refute req.key?('force')
  end

  def test_get_with_flags_omits_flags_by_default
    @server.enqueue('{"ok":true,"data":"main","age_ms":0,"stale":false}')
    @client.get_with_flags('git.branch')
    req = @server.requests.last
    refute req.key?('force')
    refute req.key?('wait')
  end

  def test_get_with_flags_returns_result
    @server.enqueue('{"ok":true,"data":"main","age_ms":55,"stale":false}')
    result = @client.get_with_flags('git.branch')
    assert_instance_of Beachcomber::Result, result
    assert_equal 'main', result.data
    assert_equal 55,     result.age_ms
  end

  # --- watch ---

  def test_watch_sends_watch_op
    # The watch stream stays open; we use the MockServer's streaming behaviour.
    # Build a minimal response then close the stream immediately.
    @server.enqueue('{"ok":true,"data":"main","age_ms":10,"stale":false}')
    stream = @client.watch('git.branch')
    event = stream.next_event
    stream.close

    req = @server.requests.last
    assert_equal 'watch',      req['op']
    assert_equal 'git.branch', req['key']
    assert_instance_of Beachcomber::WatchEvent, event
    assert_equal 'main', event.data
    assert_equal 10,     event.age_ms
    refute event.stale
  end

  def test_watch_with_path_includes_path
    @server.enqueue('{"ok":true,"data":"main","age_ms":0,"stale":false}')
    stream = @client.watch('git.branch', path: '/repo')
    stream.next_event
    stream.close
    req = @server.requests.last
    assert_equal '/repo', req['path']
  end

  def test_watch_stream_is_enumerable
    assert_includes Beachcomber::WatchStream.ancestors, Enumerable
  end

  def test_watch_stream_each_yields_watch_events
    # Build a fake socket that delivers 3 watch events then EOF.
    lines = 3.times.map { '{"ok":true,"data":"val","age_ms":1,"stale":false}' }.join("\n") + "\n"
    fake_io = StringIO.new(lines)
    stream  = Beachcomber::WatchStream.new(fake_io)
    events  = stream.to_a
    assert_equal 3, events.size
    events.each { |e| assert_instance_of Beachcomber::WatchEvent, e }
  end

  def test_watch_event_stale_flag
    @server.enqueue('{"ok":true,"data":"old","age_ms":9000,"stale":true}')
    stream = @client.watch('git.branch')
    event = stream.next_event
    stream.close
    assert event.stale
  end

  # --- session additions ---

  def test_session_hello
    @server.enqueue('{"ok":true,"data":{"protocol_version":"2","daemon_version":"1.0.0"}}')
    result = nil
    @client.session do |s|
      result = s.hello
    end
    assert_instance_of Beachcomber::HelloInfo, result
    assert_equal '2',     result.protocol_version
    assert_equal '1.0.0', result.daemon_version
  end

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

  def test_session_introspect
    @server.enqueue('{"ok":true,"data":{"items":[]}}')
    result = nil
    @client.session do |s|
      result = s.introspect('cache')
    end
    assert_instance_of Beachcomber::IntrospectResponse, result
    assert_equal 'cache', result.subject
  end

  def test_session_status
    @server.enqueue('{"ok":true,"data":[{"provider":"git","field":"branch","path":null,"value":"main","age_ms":50,"stale":false}]}')
    rows = nil
    @client.session do |s|
      rows = s.status
    end
    assert_equal 1, rows.size
    assert_instance_of Beachcomber::CacheRow, rows.first
  end

  def test_session_get_with_flags
    @server.enqueue('{"ok":true,"data":"main","age_ms":0,"stale":false}')
    result = nil
    @client.session do |s|
      result = s.get_with_flags('git.branch', force: true)
    end
    assert_instance_of Beachcomber::Result, result
    assert_equal 'main', result.data
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
          value: 'main',
          age_ms: 100,
          stale: false,
          kind: { kind: 'lifecycle', decay: 0, watches_files: true },
          poll_interval_secs: 30,
          keep_alive_polls: 3,
          fsevents_reinstate: false,
        }
      ]
    }.to_json
    @server.enqueue(payload)
    rows = @client.status
    git = rows.find { |r| r.provider == 'git' }
    refute_nil git.kind
    assert_equal 'lifecycle', git.kind['kind']
    assert git.poll_interval_secs > 0
    assert git.keep_alive_polls > 0
    refute_nil git.fsevents_reinstate
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
