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

  # --- poke ---

  def test_poke_sends_correct_op
    @server.enqueue('{"ok":true}')
    @client.poke('git')
    req = @server.requests.last
    assert_equal 'poke', req['op']
    assert_equal 'git',  req['key']
  end

  def test_poke_with_path
    @server.enqueue('{"ok":true}')
    @client.poke('git', path: '/repo')
    req = @server.requests.last
    assert_equal '/repo', req['path']
  end

  def test_poke_returns_nil
    @server.enqueue('{"ok":true}')
    result = @client.poke('git')
    assert_nil result
  end

  # --- list ---

  def test_list_sends_op
    payload = [{ 'name' => 'git', 'global' => false, 'fields' => ['branch', 'dirty'] }]
    @server.enqueue(JSON.generate({ ok: true, data: payload }))
    result = @client.list
    assert result.hit?
    assert_equal 'git', result.data.first['name']
    assert_equal 'list', @server.requests.last['op']
  end

  # --- status ---

  def test_status_sends_op
    @server.enqueue('{"ok":true,"data":{"queue_depth":0}}')
    result = @client.status
    assert result.hit?
    assert_equal 'status', @server.requests.last['op']
  end

  # --- connection error ---

  def test_daemon_not_running_when_no_socket
    client = Beachcomber::Client.new(socket_path: '/tmp/beachcomber-no-such-socket-xyz/sock')
    assert_raises(Beachcomber::DaemonNotRunning) { client.get('git.branch') }
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

  def test_list_op_field
    @server.enqueue('{"ok":true,"data":[]}')
    @client.list
    assert_equal 'list', @server.requests.last['op']
  end

  def test_status_op_field
    @server.enqueue('{"ok":true,"data":{}}')
    @client.status
    assert_equal 'status', @server.requests.last['op']
  end
end
