require_relative 'test_helper'
require 'fileutils'

# Loading the library: the required symbols, and a loud failure naming
# every candidate and why it failed. The candidates themselves are
# test_discovery.rb's, against the shared fixture.
class TestFFI < Minitest::Test
  def setup
    @orig_handle  = Beachcomber::FFI.instance_variable_get(:@handle)
    @orig_path    = Beachcomber::FFI.instance_variable_get(:@library_path)
    @orig_fn      = Beachcomber::FFI.instance_variable_get(:@fn)
    @orig_lib_env = ENV['BEACHCOMBER_LIB']
    @orig_path_env = ENV['PATH']
  end

  def teardown
    Beachcomber::FFI.instance_variable_set(:@handle, @orig_handle)
    Beachcomber::FFI.instance_variable_set(:@library_path, @orig_path)
    Beachcomber::FFI.instance_variable_set(:@fn, @orig_fn)
    ENV['BEACHCOMBER_LIB'] = @orig_lib_env
    ENV['PATH'] = @orig_path_env
  end

  def reset_memoized_load!
    Beachcomber::FFI.instance_variable_set(:@handle, nil)
  end

  def test_lib_basename_matches_platform
    expected = (RbConfig::CONFIG['host_os'] =~ /darwin/i) ? 'libbeachcomber.dylib' : 'libbeachcomber.so'
    assert_equal expected, Beachcomber::FFI::LIB_BASENAME
  end

  def test_required_symbols_covers_every_op_group
    lifecycle = %w[bc_version bc_client_new bc_client_free bc_string_free]
    ops       = %w[bc_get bc_put bc_refresh bc_status bc_introspect bc_hello]
    resolution = %w[bc_eval]
    sessions  = %w[bc_session_open bc_session_get bc_session_put bc_session_set_context bc_session_close]
    watch     = %w[bc_watch_open bc_watch_next bc_watch_cancel bc_watch_free]

    (lifecycle + ops + resolution + sessions + watch).each do |sym|
      assert_includes Beachcomber::FFI::REQUIRED_SYMBOLS, sym
    end
  end

  # An explicit $BEACHCOMBER_LIB is the only candidate: when it does not
  # load, the failure says why, and nothing else was tried.
  def test_an_explicit_library_that_does_not_load_is_the_failure
    reset_memoized_load!
    ENV['BEACHCOMBER_LIB'] = '/no/such/file.dylib'

    err = assert_raises(Beachcomber::LibraryNotFoundError) { Beachcomber::FFI.load! }
    assert_includes err.message, '$BEACHCOMBER_LIB (/no/such/file.dylib): '
    refute_includes err.message, 'the system loader'
  ensure
    reset_memoized_load!
  end

  # With nothing set, every candidate is named with why it failed.
  def test_load_names_every_candidate_and_why
    reset_memoized_load!
    Dir.mktmpdir('bcdisc', '/tmp') do |made|
      root = File.realpath(made)
      FileUtils.mkdir_p(File.join(root, 'bin'))
      File.write(File.join(root, 'bin', 'comb'), "#!/bin/sh\n")
      File.chmod(0o755, File.join(root, 'bin', 'comb'))
      ENV['PATH'] = File.join(root, 'bin')
      ENV.delete('BEACHCOMBER_LIB')
      candidates = Beachcomber::FFI.candidates('libbeachcomber-none.so')
      err = assert_raises(Beachcomber::LibraryNotFoundError) { Beachcomber::FFI.open_first(candidates) }
      assert_includes err.message, "beside comb (#{root}/bin/comb) (#{root}/lib/libbeachcomber-none.so): "
      assert_includes err.message, 'the system loader (libbeachcomber-none.so): '
    end
  ensure
    reset_memoized_load!
  end

  def test_load_succeeds_via_beachcomber_lib_env
    lib = ENV['BEACHCOMBER_LIB']
    skip 'BEACHCOMBER_LIB not pointed at a built library' unless lib && File.exist?(lib)

    reset_memoized_load!
    Beachcomber::FFI.load!
    assert_equal lib, Beachcomber::FFI.library_path
    refute_empty Beachcomber::FFI.version
  ensure
    reset_memoized_load!
  end
end
