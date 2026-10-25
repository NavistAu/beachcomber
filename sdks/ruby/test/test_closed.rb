require_relative 'test_helper'

# A closed client or session is an IoError on any later call, raised before
# the library is reached; a closed watch stream has ended
# (docs/sdk-api.md: a closed handle).
class TestClosed < Minitest::Test
  def test_a_closed_client_is_an_io_error
    client = Beachcomber::Client.new(socket_path: '/tmp/bc-closed.sock', autostart: false)
    client.close
    err = assert_raises(Beachcomber::IoError) { client.hello }
    assert_equal 'the client is closed', err.message
  end

  def test_a_closed_session_is_an_io_error
    client = Beachcomber::Client.new(socket_path: '/tmp/bc-closed.sock', autostart: false)
    session = nil
    client.session { |s| session = s }
    err = assert_raises(Beachcomber::IoError) { session.get('p.f') }
    assert_equal 'the session is closed', err.message
  ensure
    client&.close
  end

  def test_a_closed_watch_stream_has_ended
    client = Beachcomber::Client.new(socket_path: '/tmp/bc-closed.sock', autostart: false)
    stream = client.watch('p.f')
    stream.close
    assert_nil stream.next_event
    stream.cancel
  ensure
    client&.close
  end
end
