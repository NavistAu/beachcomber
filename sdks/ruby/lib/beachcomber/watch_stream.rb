module Beachcomber
  # Iterates events from a bc_watch_open handle. Create via
  # {Client#watch} rather than directly.
  #
  # Each event is the *resolved value* of the watched key — what `comb get`
  # would print for it — re-resolved whenever any key the resolution reads
  # changes, including the provider's template tree.
  #
  # bc_watch_next distinguishes five outcomes: event, timeout, eof,
  # cancelled, error. #next_event folds timeout/eof/cancelled into a single
  # nil (matching the previous socket-based API's "nil means the stream is
  # over" contract) and raises the idiomatic exception for error. Pass an
  # explicit +timeout_ms+ to observe timeouts distinctly.
  class WatchStream
    include Enumerable

    def initialize(handle)
      @handle = handle
      @closed = false
    end

    # Yields a WatchEvent per emitted change until the stream ends.
    def each
      return enum_for(:each) unless block_given?

      while (event = next_event)
        yield event
      end
    end

    # Waits for the next event. +timeout_ms+: -1 (default) blocks
    # indefinitely, 0 polls once, >0 waits that long.
    #
    # @return [WatchEvent, nil] the resolved value on an emitted change;
    #   nil on end-of-stream (daemon closed the connection, the watch was
    #   cancelled, or the wait elapsed). An event whose +data+ is nil is a
    #   miss, not the end of the stream.
    def next_event(timeout_ms = -1)
      json = Beachcomber::FFI.raw_call(:bc_watch_next, @handle, timeout_ms)
      envelope = JSON.parse(json)

      unless envelope['ok']
        err = envelope['error'] || {}
        Beachcomber.raise_for_error(err['kind'] || 'server_error', err['message'] || 'unknown error')
      end

      case envelope['outcome']
      when 'event'
        WatchEvent.new(data: envelope['data'])
      when 'timeout', 'eof', 'cancelled'
        nil
      else
        raise Beachcomber::ProtocolError, "unknown watch outcome: #{envelope['outcome'].inspect}"
      end
    end

    # Unblocks a pending or future #next_event call. Safe to call from
    # another thread while a call is in flight.
    def cancel
      Beachcomber::FFI.cancel_watch(@handle)
      nil
    end

    def close
      return if @closed

      @closed = true
      Beachcomber::FFI.free_watch(@handle)
    end
  end
end
