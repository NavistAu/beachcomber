module Beachcomber
  # Base class for all Beachcomber errors.
  class Error < StandardError; end

  # Raised when the daemon socket cannot be reached.
  class DaemonNotRunning < Error
    def initialize(socket_path)
      super("beachcomber daemon is not running (socket: #{socket_path})")
    end
  end

  # Raised when the daemon responds with ok: false.
  class ServerError < Error
    attr_reader :message

    def initialize(message)
      @message = message
      super("beachcomber: daemon error: #{message}")
    end
  end

  # Raised when a response cannot be parsed.
  class ProtocolError < Error
    def initialize(detail)
      super("beachcomber: protocol error: #{detail}")
    end
  end
end
