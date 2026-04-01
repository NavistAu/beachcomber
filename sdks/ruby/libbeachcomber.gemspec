Gem::Specification.new do |s|
  s.name        = 'libbeachcomber'
  s.version     = '0.1.0'
  s.summary     = 'Ruby client for the beachcomber shell-data daemon'
  s.description = 'Communicates with the beachcomber daemon over a Unix domain socket ' \
                  'to query cached shell-environment data (git state, hostname, battery, etc.).'
  s.authors     = ['NavistAu']
  s.license     = 'MIT'
  s.homepage    = 'https://github.com/NavistAu/beachcomber'

  s.required_ruby_version = '>= 3.0'

  s.files         = Dir['lib/**/*.rb']
  s.require_paths = ['lib']

  s.metadata = {
    'source_code_uri'   => 'https://github.com/NavistAu/beachcomber/tree/main/sdks/ruby',
    'bug_tracker_uri'   => 'https://github.com/NavistAu/beachcomber/issues',
    'changelog_uri'     => 'https://github.com/NavistAu/beachcomber/blob/main/CHANGELOG.md',
  }
end
