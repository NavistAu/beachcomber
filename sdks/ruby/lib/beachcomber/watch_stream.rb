module Beachcomber
  # Iterates events from a bc_watch_open handle. Create via
  # {Client#watch} rather than directly.
  #
  # Each event is the value of the watched key — what `comb get` would print
  # for it — re-resolved whenever any key the resolution reads changes,
  # including the provider's template tree.
  #
  # bc_watch_next has five outcomes: event, timeout, eof, cancelled, error.
  # #next_event answers a WatchEvent for an event, nil once the stream has
  # ended (eof or cancelled), and raises: TimeoutError when its wait runs
  # out, the error's own class for an error.
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

    # Waits for the next event: indefinitely when +timeout_ms+ is nil, else
    # that long (0 polls once).
    #
    # @return [WatchEvent, nil] the value on an emitted change; nil once the
    #   stream has ended (the daemon closed it, or it was cancelled). An
    #   event whose +data+ is nil is a miss, not the end of the stream.
    # @raise [Beachcomber::TimeoutError] when +timeout_ms+ runs out with no
    #   event; the stream is still open
    def next_event(timeout_ms = nil)
      return nil if @closed

      json = Beachcomber::FFI.raw_call(:bc_watch_next, @handle, timeout_ms || -1)
      envelope = JSON.parse(json)

      unless envelope['ok']
        err = envelope['error'] || {}
        Beachcomber.raise_for_error(err['kind'] || 'server_error', err['message'] || 'unknown error')
      end

      case envelope['outcome']
      when 'event'
        WatchEvent.new(data: envelope['data'])
      when 'eof', 'cancelled'
        nil
      when 'timeout'
        raise Beachcomber::TimeoutError, "no event within #{timeout_ms}ms"
      else
        raise Beachcomber::ParseError, "unknown watch outcome: #{envelope['outcome'].inspect}"
      end
    end

    # Ends the stream: a pending or later #next_event answers nil. Safe to
    # call from another thread while a call is in flight.
    def cancel
      Beachcomber::FFI.cancel_watch(@handle) unless @closed
      nil
    end

    # Cancels the stream and frees its handle. Idempotent.
    def close
      return if @closed

      @closed = true
      Beachcomber::FFI.cancel_watch(@handle)
      Beachcomber::FFI.free_watch(@handle)
    end
  end
end
