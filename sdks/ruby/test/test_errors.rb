require_relative 'test_helper'

class TestErrors < Minitest::Test
  KIND_MAP = {
    'bad_flags' => Beachcomber::BadFlagsError,
    'busy' => Beachcomber::BusyError,
    'panic' => Beachcomber::PanicError,
    'version_skew' => Beachcomber::VersionSkewError,
    'daemon_not_running' => Beachcomber::DaemonNotRunning,
    'connection_failed' => Beachcomber::ConnectionFailedError,
    'io_error' => Beachcomber::IoError,
    'parse_error' => Beachcomber::ProtocolError,
    'server_error' => Beachcomber::ServerError,
    'timeout' => Beachcomber::TimeoutError,
  }.freeze

  def test_every_known_kind_maps_to_its_class
    KIND_MAP.each do |kind, klass|
      err = assert_raises(klass) { Beachcomber.raise_for_error(kind, 'boom') }
      assert_equal kind, err.kind
      assert_kind_of Beachcomber::CombError, err
    end
  end

  def test_every_mapped_class_is_a_comb_error
    KIND_MAP.each_value { |klass| assert_operator klass, :<, Beachcomber::CombError }
  end

  def test_unknown_kind_still_raises_with_kind_preserved
    err = assert_raises(Beachcomber::CombError) { Beachcomber.raise_for_error('something_new', 'boom') }
    assert_equal 'something_new', err.kind
  end

  def test_message_includes_the_underlying_text
    err = assert_raises(Beachcomber::ServerError) { Beachcomber.raise_for_error('server_error', 'unknown provider: foo') }
    assert_includes err.message, 'unknown provider: foo'
  end

  def test_message_includes_kind
    err = assert_raises(Beachcomber::TimeoutError) { Beachcomber.raise_for_error('timeout', 'took too long') }
    assert_includes err.message, 'kind=timeout'
  end
end
