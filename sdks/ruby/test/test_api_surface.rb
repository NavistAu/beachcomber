require_relative 'test_helper'

# The SDK API table (docs/sdk-api.md), checked against sdks/api.json: this
# SDK's public surface is the table's, name for name, and nothing else.
class TestApiSurface < Minitest::Test
  API = JSON.parse(File.read(File.expand_path('../../api.json', __dir__)))

  # A kind slug's class name: `daemon_not_running` -> `DaemonNotRunningError`,
  # `io_error` -> `IoError`.
  def self.error_class_name(kind)
    name = kind.split('_').map(&:capitalize).join
    name.end_with?('Error') ? name : "#{name}Error"
  end

  def assert_methods(expected, klass, idiom: [])
    actual = klass.public_instance_methods(false).map(&:to_s) - idiom
    assert_equal expected.sort, actual.sort, "#{klass}'s public methods"
  end

  def test_client_methods_are_the_tables
    assert_methods API['client']['methods'], Beachcomber::Client
  end

  def test_client_options_are_the_tables
    params = Beachcomber::Client.instance_method(:initialize).parameters
    assert_equal API['client']['options'].sort, params.map { |_, name| name.to_s }.sort
    assert(params.all? { |type, _| type == :key }, "every option is a keyword: #{params.inspect}")
  end

  def test_session_methods_are_the_tables
    assert_methods API['session']['methods'], Beachcomber::Session
  end

  def test_watch_stream_methods_are_the_tables
    assert_methods API['watch_stream']['methods'], Beachcomber::WatchStream, idiom: %w[each]
  end

  def test_one_error_class_per_kind_named_for_it
    classes = Beachcomber.constants.map { |c| Beachcomber.const_get(c) }
                         .select { |c| c.is_a?(Class) && c < Beachcomber::CombError }
    assert_equal API['error_kinds'].sort, classes.map(&:kind).sort
    API['error_kinds'].each do |kind|
      klass = Beachcomber.const_get(self.class.error_class_name(kind))
      assert_equal kind, klass.kind
      assert_operator klass, :<, Beachcomber::CombError
    end
  end

  def test_the_error_base_is_a_standard_error
    assert_equal StandardError, Beachcomber::CombError.superclass
    refute Beachcomber.const_defined?(:Error, false), 'one base: CombError'
  end

  def test_every_error_carries_the_tables_fields
    API['error_kinds'].each do |kind|
      err = Beachcomber.const_get(self.class.error_class_name(kind)).new('boom')
      API['error_fields'].each { |field| assert_respond_to err, field }
      assert_equal kind, err.kind
      assert_equal 'boom', err.message
    end
  end

  def test_the_typed_shapes_have_exactly_the_tables_fields
    {
      'cache_row' => Beachcomber::CacheRow,
      'hello_info' => Beachcomber::HelloInfo,
      'daemon_health' => Beachcomber::DaemonHealth,
      'reaper_status' => Beachcomber::ReaperStatus,
      'verdict' => Beachcomber::Verdict,
      'introspect_response' => Beachcomber::IntrospectResponse,
      'watch_event' => Beachcomber::WatchEvent,
    }.each do |name, struct|
      assert_equal API['types'][name].sort, struct.members.map(&:to_s).sort, name
    end
  end
end
