require 'etc'

module Beachcomber
  # Discovers the Unix socket path for the beachcomber daemon.
  #
  # Discovery order:
  #   1. $XDG_RUNTIME_DIR/beachcomber/sock  (if set and the path exists)
  #   2. $TMPDIR/beachcomber-<uid>/sock
  #   3. /tmp/beachcomber-<uid>/sock
  module Discovery
    # @return [String] the discovered or best-guess socket path
    def self.socket_path
      xdg = ENV['XDG_RUNTIME_DIR']
      if xdg && !xdg.empty?
        candidate = File.join(xdg, 'beachcomber', 'sock')
        return candidate if File.exist?(candidate)
      end

      uid  = Process.uid
      dir  = "beachcomber-#{uid}"
      base = ENV.fetch('TMPDIR', nil) || '/tmp'
      File.join(base, dir, 'sock')
    end
  end
end
