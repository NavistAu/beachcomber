require 'etc'

module Beachcomber
  # Discovers the Unix socket path for the beachcomber daemon.
  #
  # Mirrors the daemon's bind-path resolution (Config::resolve_socket_path),
  # minus the config-file step which is daemon-only. Discovery order:
  #   1. $BEACHCOMBER_SOCKET  (if set and non-empty)
  #   2. /tmp/beachcomber-<uid>/sock
  #
  # There is no existence probe and no session-scoped environment is
  # consulted ($TMPDIR, $XDG_RUNTIME_DIR): the result is the single path the
  # daemon binds for the same environment. Non-standard setups point clients
  # at the daemon via BEACHCOMBER_SOCKET.
  module Discovery
    # @return [String] the resolved socket path
    def self.socket_path
      sock = ENV['BEACHCOMBER_SOCKET']
      return sock if sock && !sock.empty?

      File.join('/tmp', "beachcomber-#{Process.uid}", 'sock')
    end
  end
end
