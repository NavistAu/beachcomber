require_relative 'test_helper'

class TestClient < Minitest::Test
  def setup
    @server = MockServer.new
    @client = Beachcomber::Client.new(socket_path: @server.path)
  end

  def teardown
    @server.stop
  end

  # --- get: the value `comb get` prints ---

  def test_get_is_the_value
    @server.enqueue('{"ok":true,"data":"main","age_ms":42,"stale":false}')
    assert_equal 'main', @client.get('git.branch')
  end

  def test_get_miss_is_nil
    @server.enqueue('{"ok":true}')
    assert_nil @client.get('git.branch')
  end

  def test_get_null_data_is_a_miss
    @server.enqueue('{"ok":true,"data":null}')
    assert_nil @client.get('git.branch')
  end

  def test_get_reads_at_cwd
    @server.enqueue('{"ok":true,"data":"main","age_ms":5,"stale":false}')
    @client.get('git.branch', cwd: '/repo')
    req = @server.requests.last
    assert_equal 'get',    req['op']
    assert_equal 'git.branch', req['key']
    assert_equal '/repo',  req['path']
  end

  def test_get_without_cwd_is_the_global_entry
    @server.enqueue('{"ok":true}')
    @client.get('git.branch')
    req = @server.requests.last
    refute req.key?('path')
  end

  def test_get_sends_the_force_and_wait_flags
    @server.enqueue('{"ok":true,"data":"main","age_ms":5,"stale":false}')
    @client.get('git.branch', force: true, wait: true)
    req = @server.requests.last
    assert_equal true, req['force']
    assert_equal true, req['wait']
  end

  def test_get_hash_data
    @server.enqueue('{"ok":true,"data":{"branch":"main","dirty":false},"age_ms":1,"stale":false}')
    assert_equal({ 'branch' => 'main', 'dirty' => false }, @client.get('git'))
  end

  # --- a template renders, with the caller's environment ---

  def test_get_renders_a_template_only_node
    @server.enqueue('{"ok":true,"template":"{{ env.A }}"}')
    assert_equal 'x', @client.get('fx.t', env: { 'A' => 'x' })
  end

  def test_get_a_template_wins_over_the_value_beside_it
    @server.enqueue('{"ok":true,"data":1,"age_ms":12,"stale":false,"template":"{{ 2 }}"}')
    assert_equal 2, @client.get('fx.v')
  end

  def test_get_renders_a_template_group
    @server.enqueue('{"ok":true,"template":{"foo":"{{ 1 }}"}}')
    assert_equal({ 'foo' => 1 }, @client.get('fx.widgets'))
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

  # The daemon's refusal reaches the caller in its own words — the same text
  # `comb refresh` prints — directly after this SDK's own prefix, with no
  # `server error: ` from the envelope between them.
  def test_refresh_refusal_is_the_daemons_words
    @server.enqueue('{"ok":false,"error":"nothing to refresh: sp has no sources"}')
    err = assert_raises(Beachcomber::ServerError) { @client.refresh('sp') }
    assert err.message.start_with?('beachcomber: nothing to refresh: sp has no sources'), err.message
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

  # --- exec option ---
  # An exec() call with no daemon-backed ref never touches the socket, so
  # the option's effect is observable against the mock alone.

  def test_exec_is_disabled_by_default
    err = assert_raises(Beachcomber::ServerError) do
      @client.eval_expression("{{ exec(['printf', 'x']) }}", cwd: '/tmp')
    end
    assert_includes err.message, 'exec() is disabled in this context'
  end

  def test_exec_option_reaches_the_library
    client = Beachcomber::Client.new(socket_path: @server.path, exec: true)
    assert_equal 'x', client.eval_expression("{{ exec(['printf', 'x']) }}", cwd: '/tmp')
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
      assert_equal 'main', s.get('git.branch')
    end
  end

  # set_context is the session's own: nothing reaches the daemon for it,
  # and the directory rides along on the next request that names none.
  def test_session_set_context
    @server.enqueue('{"ok":true,"data":"main","age_ms":1,"stale":false}')

    @client.session do |s|
      s.set_context('/repo')
      assert_equal 'main', s.get('git.branch')
    end

    assert_equal 1, @server.requests.length
    get_req = @server.requests.first
    assert_equal 'get',   get_req['op']
    assert_equal '/repo', get_req['path']
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
    assert_equal 'x', @client.get('git.branch')
  end

  def test_session_returns_block_value
    @server.enqueue('{"ok":true,"data":"main","age_ms":1,"stale":false}')
    value = @client.session { |s| s.get('git.branch') }
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
