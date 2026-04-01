Gem::Specification.new do |s|
  s.name        = 'beachcomber'
  s.version     = '0.1.0'
  s.summary     = 'Ruby client for the beachcomber shell-data daemon'
  s.description = 'Communicates with the beachcomber daemon over a Unix domain socket ' \
                  'to query cached shell-environment data (git state, hostname, battery, …).'
  s.authors     = ['beachcomber contributors']
  s.license     = 'MIT'

  s.required_ruby_version = '>= 3.0'

  s.files         = Dir['lib/**/*.rb']
  s.require_paths = ['lib']

  # stdlib only — no runtime dependencies
end
