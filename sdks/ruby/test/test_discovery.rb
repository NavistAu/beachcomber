require_relative 'test_helper'

class TestDiscovery < Minitest::Test
  # Preserve and restore environment modifications around each test.
  def setup
    @orig_sock   = ENV['BEACHCOMBER_SOCKET']
    @orig_xdg    = ENV['XDG_RUNTIME_DIR']
    @orig_tmpdir = ENV['TMPDIR']
  end

  def teardown
    ENV['BEACHCOMBER_SOCKET'] = @orig_sock
    ENV['XDG_RUNTIME_DIR']    = @orig_xdg
    ENV['TMPDIR']             = @orig_tmpdir
  end

  def test_beachcomber_socket_takes_precedence
    ENV['BEACHCOMBER_SOCKET'] = '/custom/path/comb.sock'
    ENV['XDG_RUNTIME_DIR']    = '/run/user/1000'
    ENV['TMPDIR']             = '/should-not-be-used'

    assert_equal '/custom/path/comb.sock', Beachcomber::Discovery.socket_path
  end

  def test_xdg_runtime_dir_is_ignored
    # No session-scoped environment influences resolution: XDG_RUNTIME_DIR
    # is ignored even when set, and the result falls back to the per-user
    # default, matching where the daemon binds.
    ENV.delete('BEACHCOMBER_SOCKET')
    ENV['XDG_RUNTIME_DIR'] = '/run/user/1000'
    ENV['TMPDIR']          = '/should-not-be-used'

    result = Beachcomber::Discovery.socket_path
    assert_equal "/tmp/beachcomber-#{Process.uid}/sock", result
  end

  def test_xdg_runtime_dir_ignored_when_empty
    ENV.delete('BEACHCOMBER_SOCKET')
    ENV['XDG_RUNTIME_DIR'] = ''
    ENV['TMPDIR']          = '/should-not-be-used'

    result = Beachcomber::Discovery.socket_path
    assert_equal "/tmp/beachcomber-#{Process.uid}/sock", result
  end

  def test_xdg_runtime_dir_ignored_when_unset
    ENV.delete('BEACHCOMBER_SOCKET')
    ENV.delete('XDG_RUNTIME_DIR')
    ENV['TMPDIR'] = '/should-not-be-used'

    result = Beachcomber::Discovery.socket_path
    assert_equal "/tmp/beachcomber-#{Process.uid}/sock", result
  end

  def test_tmpdir_is_ignored
    # TMPDIR must never influence resolution.
    ENV.delete('BEACHCOMBER_SOCKET')
    ENV.delete('XDG_RUNTIME_DIR')
    ENV['TMPDIR'] = '/var/folders/xyz'

    result = Beachcomber::Discovery.socket_path
    assert_equal "/tmp/beachcomber-#{Process.uid}/sock", result
  end

  def test_falls_back_to_slash_tmp
    ENV.delete('BEACHCOMBER_SOCKET')
    ENV.delete('XDG_RUNTIME_DIR')
    ENV.delete('TMPDIR')

    result = Beachcomber::Discovery.socket_path
    assert_equal "/tmp/beachcomber-#{Process.uid}/sock", result
  end

  def test_uid_embedded_in_path
    ENV.delete('BEACHCOMBER_SOCKET')
    ENV.delete('XDG_RUNTIME_DIR')

    result = Beachcomber::Discovery.socket_path
    assert_includes result, Process.uid.to_s
  end
end
