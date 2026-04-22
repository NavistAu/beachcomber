module Beachcomber
  class WatchStream
    include Enumerable

    def initialize(socket)
      @socket = socket
    end

    # Yields a WatchEvent per emitted change.
    def each
      return enum_for(:each) unless block_given?
      while (line = @socket.gets)
        line.strip!
        next if line.empty?
        resp = JSON.parse(line)
        unless resp["ok"]
          raise ServerError, resp["error"] || "watch error"
        end
        yield WatchEvent.new(
          data:   resp["data"],
          age_ms: Integer(resp["age_ms"] || 0),
          stale:  resp["stale"] == true,
        )
      end
    end

    # Read the next event; returns nil on connection close.
    def next_event
      loop do
        line = @socket.gets
        return nil if line.nil?
        line.strip!
        next if line.empty?
        resp = JSON.parse(line)
        unless resp["ok"]
          raise ServerError, resp["error"] || "watch error"
        end
        return WatchEvent.new(
          data:   resp["data"],
          age_ms: Integer(resp["age_ms"] || 0),
          stale:  resp["stale"] == true,
        )
      end
    end

    def close
      @socket.close
    end
  end
end
