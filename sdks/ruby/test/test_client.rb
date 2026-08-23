require_relative 'test_helper'

class TestClient < Minitest::Test
  def setup
    @server = MockServer.new
    @client = Beachcomber::Client.new(socket_path: @server.path)
  end

  def teardown
    @server.stop
  end

  # --- get ---

  def test_get_hit
    @server.enqueue('{"ok":true,"data":"main","age_ms":42,"stale":false}')
    result = @client.get('git.branch')
    assert result.hit?
    assert_equal 'main', result.data
    assert_equal 42,     result.age_ms
    refute result.stale?
  end

  def test_get_miss
    @server.enqueue('{"ok":true}')
    result = @client.get('git.branch')
    assert result.miss?
    assert_nil result.data
  end

  def test_get_null_data_treated_as_miss
    @server.enqueue('{"ok":true,"data":null}')
    result = @client.get('git.branch')
    assert result.miss?
  end

  def test_get_with_path
    @server.enqueue('{"ok":true,"data":"main","age_ms":5,"stale":false}')
    @client.get('git.branch', path: '/repo')
    req = @server.requests.last
    assert_equal 'get',    req['op']
    assert_equal 'git.branch', req['key']
    assert_equal '/repo',  req['path']
  end

  def test_get_without_path_omits_field
    @server.enqueue('{"ok":true}')
    @client.get('git.branch')
    req = @server.requests.last
    refute req.key?('path')
  end

  def test_get_hash_data
    @server.enqueue('{"ok":true,"data":{"branch":"main","dirty":false},"age_ms":1,"stale":false}')
    result = @client.get('git')
    assert result.hit?
    assert_equal 'main',  result['branch']
    assert_equal false,   result['dirty']
  end

  def test_get_stale_flag
    @server.enqueue('{"ok":true,"data":"old","age_ms":9000,"stale":true}')
    result = @client.get('git.branch')
    assert result.stale?
  end

  # --- server error ---

  def test_get_server_error_raises
    @server.enqueue('{"ok":false,"error":"unknown provider: foo"}')
    err = assert_raises(Beachcomber::ServerError) { @client.get('foo.bar') }
    assert_match(/unknown provider: foo/, err.message)
  end

  # --- refresh ---

  def test_refresh_sends_correct_op
    @server.enqueue('{"ok":true}')
    @client.refresh('git')
    req = @server.requests.last
    assert_equal 'refresh', req['op']
    assert_equal 'git',     req['key']
  end

  def test_refresh_with_path
    @server.enqueue('{"ok":true}')
    @client.refresh('git', path: '/repo')
    req = @server.requests.last
    assert_equal '/repo', req['path']
  end

  def test_refresh_returns_nil
    @server.enqueue('{"ok":true}')
    result = @client.refresh('git')
    assert_nil result
  end

  # --- status ---

  def test_status_sends_op
    @server.enqueue('{"ok":true,"data":[]}')
    rows = @client.status
    assert_equal [], rows
    assert_equal 'status', @server.requests.last['op']
  end

  # --- connection error ---

  # An explicit-but-unreachable socket_path surfaces as
  # Beachcomber::ConnectionFailedError (the shared client's
  # CombError::ConnectionFailed) rather than DaemonNotRunning, which is
  # reserved for the no-override, autostart-disabled default-discovery path.
  # Both share the CombError ancestor and carry a `kind`.
  def test_daemon_not_running_when_no_socket
    client = Beachcomber::Client.new(socket_path: '/tmp/beachcomber-no-such-socket-xyz/sock')
    err = assert_raises(Beachcomber::CombError) { client.get('git.branch') }
    assert_includes %w[connection_failed daemon_not_running], err.kind
  end

  # --- protocol errors ---

  def test_malformed_json_raises_protocol_error
    @server.enqueue('not json at all')
    assert_raises(Beachcomber::ProtocolError) { @client.get('git.branch') }
  end

  # --- session ---

  def test_session_block_yields_session
    @server.enqueue('{"ok":true,"data":"main","age_ms":1,"stale":false}')
    @client.session do |s|
      assert_instance_of Beachcomber::Session, s
      r = s.get('git.branch')
      assert_equal 'main', r.data
    end
  end

  def test_session_set_context
    @server.enqueue('{"ok":true}') # context ack
    @server.enqueue('{"ok":true,"data":"main","age_ms":1,"stale":false}')

    @client.session do |s|
      s.set_context('/repo')
      r = s.get('git.branch')
      assert r.hit?
    end

    ctx_req = @server.requests.first
    assert_equal 'context', ctx_req['op']
    assert_equal '/repo',   ctx_req['path']
  end

  def test_session_multiple_requests_on_one_connection
    3.times { @server.enqueue('{"ok":true,"data":"main","age_ms":1,"stale":false}') }

    @client.session do |s|
      3.times { s.get('git.branch') }
    end

    assert_equal 3, @server.requests.size
  end

  def test_session_closes_on_exception
    @server.enqueue('{"ok":false,"error":"oops"}')
    assert_raises(Beachcomber::ServerError) do
      @client.session { |s| s.get('git.branch') }
    end
    # The server should still accept a new connection (connection was closed).
    @server.enqueue('{"ok":true,"data":"x","age_ms":0,"stale":false}')
    result = @client.get('git.branch')
    assert result.hit?
  end

  def test_session_returns_block_value
    @server.enqueue('{"ok":true,"data":"main","age_ms":1,"stale":false}')
    value = @client.session { |s| s.get('git.branch').data }
    assert_equal 'main', value
  end

  # --- each op sends correct JSON ---

  def test_get_op_field
    @server.enqueue('{"ok":true}')
    @client.get('hostname')
    assert_equal 'get', @server.requests.last['op']
  end

  def test_status_op_field
    @server.enqueue('{"ok":true,"data":[]}')
    @client.status
    assert_equal 'status', @server.requests.last['op']
  end
end
