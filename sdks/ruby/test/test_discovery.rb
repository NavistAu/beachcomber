require_relative 'test_helper'
require 'fileutils'

# Library discovery, against the shared fixture sdks/discovery.json (the rule
# in docs/sdk-api.md, "Library discovery"): every case builds its tree in a
# fresh directory and checks the candidate paths, in order. Nothing is loaded.
class TestDiscovery < Minitest::Test
  FIXTURE = JSON.parse(File.read(File.expand_path('../../discovery.json', __dir__)))

  def setup
    @orig_lib_env = ENV['BEACHCOMBER_LIB']
    @orig_path_env = ENV['PATH']
  end

  def teardown
    ENV['BEACHCOMBER_LIB'] = @orig_lib_env
    ENV['PATH'] = @orig_path_env
  end

  # +tree+ under +root+: "exe" an executable file, "file" one that is not,
  # "-> target" a symlink to target.
  def build(root, tree, lib)
    tree.each do |rel, kind|
      path = File.join(root, rel.gsub('{lib}', lib))
      FileUtils.mkdir_p(File.dirname(path))
      if kind.start_with?('-> ')
        File.symlink(kind.delete_prefix('-> '), path)
      else
        File.write(path, "#!/bin/sh\n")
        File.chmod(kind == 'exe' ? 0o755 : 0o644, path)
      end
    end
  end

  FIXTURE['cases'].each do |c|
    define_method("test_#{c['name']}") do
      lib = Beachcomber::FFI::LIB_BASENAME
      Dir.mktmpdir('bcdisc', '/tmp') do |made|
        root = File.realpath(made)
        sub = ->(s) { s.gsub('{root}', root).gsub('{lib}', lib) }
        build(root, c['tree'], lib)
        ENV['PATH'] = c['path'].map(&sub).join(File::PATH_SEPARATOR)
        if c.key?('lib_env')
          ENV['BEACHCOMBER_LIB'] = sub.(c['lib_env'])
        else
          ENV.delete('BEACHCOMBER_LIB')
        end

        assert_equal c['expect'].map(&sub), Beachcomber::FFI.candidates.map(&:path)
      end
    end
  end

  def test_the_library_file_is_dylib_on_macos_and_so_everywhere_else
    assert_equal 'libbeachcomber.dylib', Beachcomber::FFI.library_basename('darwin23')
    assert_equal 'libbeachcomber.so', Beachcomber::FFI.library_basename('linux-gnu')
    assert_equal 'libbeachcomber.so', Beachcomber::FFI.library_basename('freebsd14')
  end
end
