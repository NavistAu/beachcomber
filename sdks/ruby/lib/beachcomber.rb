require_relative 'beachcomber/errors'
require_relative 'beachcomber/ffi'
require_relative 'beachcomber/types'
require_relative 'beachcomber/watch_stream'
require_relative 'beachcomber/client'

# Beachcomber is a Ruby client for the beachcomber daemon.
#
# The daemon caches shell-environment data (git state, hostname, battery, …)
# and serves it over a Unix domain socket using newline-delimited JSON.
#
# Quick start:
#
#   require 'beachcomber'
#
#   client = Beachcomber::Client.new
#   branch = client.get('git.branch', cwd: '/path/to/repo') # nil on a miss
#
# Persistent session (one connection, multiple queries):
#
#   client.session do |s|
#     s.set_context('/path/to/repo')
#     branch = s.get('git.branch')
#     dirty = s.get('git.dirty')
#   end
module Beachcomber
  VERSION = '0.1.0'
end
