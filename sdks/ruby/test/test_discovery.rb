require_relative 'test_helper'

class TestDiscovery < Minitest::Test
  # Preserve and restore environment modifications around each test.
  def setup
    @orig_xdg    = ENV['XDG_RUNTIME_DIR']
    @orig_tmpdir = ENV['TMPDIR']
  end

  def teardown
    ENV['XDG_RUNTIME_DIR'] = @orig_xdg
    ENV['TMPDIR']          = @orig_tmpdir
  end

  def test_xdg_path_used_when_socket_exists
    Dir.mktmpdir do |dir|
      sock_dir  = File.join(dir, 'beachcomber')
      sock_path = File.join(sock_dir, 'sock')
      Dir.mkdir(sock_dir)
      FileUtils.touch(sock_path)

      ENV['XDG_RUNTIME_DIR'] = dir
      assert_equal sock_path, Beachcomber::Discovery.socket_path
    end
  end

  def test_xdg_skipped_when_socket_missing
    Dir.mktmpdir do |dir|
      # XDG dir exists but no sock inside it
      ENV['XDG_RUNTIME_DIR'] = dir
      ENV['TMPDIR']          = dir

      result = Beachcomber::Discovery.socket_path
      refute_equal File.join(dir, 'beachcomber', 'sock'), result
      assert_match(/beachcomber-#{Process.uid}/, result)
    end
  end

  def test_xdg_skipped_when_env_empty
    ENV['XDG_RUNTIME_DIR'] = ''
    ENV['TMPDIR']          = '/tmp'

    result = Beachcomber::Discovery.socket_path
    assert_equal "/tmp/beachcomber-#{Process.uid}/sock", result
  end

  def test_xdg_skipped_when_env_unset
    ENV.delete('XDG_RUNTIME_DIR')
    ENV['TMPDIR'] = '/tmp'

    result = Beachcomber::Discovery.socket_path
    assert_equal "/tmp/beachcomber-#{Process.uid}/sock", result
  end

  def test_tmpdir_used_when_set
    ENV.delete('XDG_RUNTIME_DIR')
    ENV['TMPDIR'] = '/custom/tmp'

    result = Beachcomber::Discovery.socket_path
    assert_equal "/custom/tmp/beachcomber-#{Process.uid}/sock", result
  end

  def test_falls_back_to_slash_tmp
    ENV.delete('XDG_RUNTIME_DIR')
    ENV.delete('TMPDIR')

    result = Beachcomber::Discovery.socket_path
    assert_equal "/tmp/beachcomber-#{Process.uid}/sock", result
  end

  def test_uid_embedded_in_path
    ENV.delete('XDG_RUNTIME_DIR')
    ENV['TMPDIR'] = '/tmp'

    result = Beachcomber::Discovery.socket_path
    assert_includes result, Process.uid.to_s
  end
end
