require_relative 'test_helper'
require 'fileutils'

# Exercises the library-discovery contract common to every dynamic-language
# binding (see Phase 4's header in the client-ABI plan): $BEACHCOMBER_LIB,
# then ../lib/ beside the resolved `comb` on $PATH, then the platform
# default search path — and a loud, location-naming failure when none work.
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
    ops       = %w[bc_get bc_put bc_put_null bc_refresh bc_status bc_introspect bc_hello]
    resolution = %w[bc_resolve bc_eval]
    sessions  = %w[bc_session_open bc_session_get bc_session_put bc_session_set_context bc_session_close]
    watch     = %w[bc_watch_open bc_watch_next bc_watch_cancel bc_watch_free]

    (lifecycle + ops + resolution + sessions + watch).each do |sym|
      assert_includes Beachcomber::FFI::REQUIRED_SYMBOLS, sym
    end
  end

  def test_resolved_comb_path_nil_when_absent_from_path
    Dir.mktmpdir do |dir|
      ENV['PATH'] = dir
      assert_nil Beachcomber::FFI.resolved_comb_path
    end
  end

  def test_resolved_comb_path_resolves_symlinks
    Dir.mktmpdir do |dir|
      dir = File.realpath(dir) # macOS: /tmp is itself a symlink to /private/tmp
      real_dir = File.join(dir, 'real')
      link_dir = File.join(dir, 'bin')
      FileUtils.mkdir_p(real_dir)
      FileUtils.mkdir_p(link_dir)
      real_comb = File.join(real_dir, 'comb')
      File.write(real_comb, "#!/bin/sh\n")
      FileUtils.chmod('+x', real_comb)
      File.symlink(real_comb, File.join(link_dir, 'comb'))

      ENV['PATH'] = link_dir
      assert_equal real_comb, Beachcomber::FFI.resolved_comb_path
    end
  end

  def test_candidate_beside_comb_is_parent_lib_dir
    Dir.mktmpdir do |dir|
      dir = File.realpath(dir) # macOS: /tmp is itself a symlink to /private/tmp
      bin_dir = File.join(dir, 'bin')
      FileUtils.mkdir_p(bin_dir)
      comb = File.join(bin_dir, 'comb')
      File.write(comb, "#!/bin/sh\n")
      FileUtils.chmod('+x', comb)

      ENV['PATH'] = bin_dir
      expected = File.join(dir, 'lib', Beachcomber::FFI::LIB_BASENAME)
      assert_equal expected, Beachcomber::FFI.candidate_beside_comb
    end
  end

  def test_candidate_beside_comb_nil_when_no_comb_on_path
    Dir.mktmpdir do |dir|
      ENV['PATH'] = dir
      assert_nil Beachcomber::FFI.candidate_beside_comb
    end
  end

  # Discovery failure is loud: names every location tried, in order, with
  # no silent fallback to a subprocess transport.
  def test_load_raises_library_not_found_naming_every_tried_location
    reset_memoized_load!
    Dir.mktmpdir do |dir|
      ENV['PATH'] = dir # no `comb` anywhere on PATH
      ENV['BEACHCOMBER_LIB'] = '/no/such/file.dylib'

      err = assert_raises(Beachcomber::LibraryNotFound) { Beachcomber::FFI.load! }
      assert_includes err.message, '/no/such/file.dylib'
      assert_includes err.message, Beachcomber::FFI::LIB_BASENAME
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
