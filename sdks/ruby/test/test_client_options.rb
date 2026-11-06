require_relative 'test_helper'

# The options Client.new hands bc_client_new: exactly the ones the caller
# gave, under the ABI's names, and no defaults of the SDK's own.
class TestClientOptions < Minitest::Test
  def test_an_option_not_given_is_not_sent
    assert_equal({}, JSON.parse(Beachcomber::Client.options_json))
  end

  def test_the_options_reach_the_options_json_as_given
    json = Beachcomber::Client.options_json(
      socket_path: '/tmp/s', timeout_ms: 250, connect_timeout_ms: 0, autostart: false, exec: true,
    )
    assert_equal(
      {
        'socket_path' => '/tmp/s', 'timeout_ms' => 250, 'connect_timeout_ms' => 0,
        'autostart' => false, 'exec' => true,
      },
      JSON.parse(json),
    )
  end

  def test_the_seconds_timeout_is_gone
    assert_raises(ArgumentError) { Beachcomber::Client.new(socket_path: '/tmp/s', timeout: 0.5) }
  end

  # A short path: one past the platform limit fails before any retry, which
  # would pass this test for the wrong reason.
  def test_connect_timeout_ms_bounds_the_connect
    Dir.mktmpdir('bcct', '/tmp') do |root|
      client = Beachcomber::Client.new(socket_path: "#{root}/sock", autostart: false, connect_timeout_ms: 0)
      started = Process.clock_gettime(Process::CLOCK_MONOTONIC)
      assert_raises(Beachcomber::ConnectionFailedError) { client.hello }
      assert_operator Process.clock_gettime(Process::CLOCK_MONOTONIC) - started, :<, 0.25
    end
  end
end
