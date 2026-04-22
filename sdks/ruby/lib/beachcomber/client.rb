require 'socket'
require 'json'

require_relative 'discovery'
require_relative 'errors'
require_relative 'result'

module Beachcomber
  DEFAULT_TIMEOUT = 0.1 # seconds (100 ms)

  # Session holds a persistent connection to the daemon and sends multiple
  # requests over the same socket.
  #
  # Obtain a Session via {Client#session}:
  #
  #   client.session do |s|
  #     s.set_context('/repo')
  #     r = s.get('git.branch')
  #   end
  #
  # Not thread-safe; use one session per thread.
  class Session
    def initialize(socket, timeout)
      @socket  = socket
      @timeout = timeout
    end

    # Sets the default path for subsequent queries on this connection.
    #
    # @param path [String]
    def set_context(path)
      roundtrip({ op: 'context', path: path })
      nil
    end

    # Reads a cached value.
    #
    # @param key [String] e.g. "git.branch" or "git"
    # @param path [String, nil] optional working-directory override
    # @return [Result]
    def get(key, path: nil)
      req = { op: 'get', key: key }
      req[:path] = path if path
      roundtrip(req)
    end

    # Forces the daemon to recompute a provider/key.
    #
    # @param key [String]
    # @param path [String, nil]
    def refresh(key, path: nil)
      req = { op: 'refresh', key: key }
      req[:path] = path if path
      roundtrip(req)
      nil
    end

    # Lists available providers.
    #
    # @return [Result]
    def list
      roundtrip({ op: 'list' })
    end

    # Returns daemon status.
    #
    # @return [Result]
    def status
      roundtrip({ op: 'status' })
    end

    # Closes the underlying socket connection.
    def close
      @socket.close unless @socket.closed?
    end

    private

    def roundtrip(req)
      line = JSON.generate(req) + "\n"
      @socket.write(line)
      raw = @socket.gets
      raise ProtocolError, "connection closed before response" if raw.nil?

      parse_response(raw.chomp)
    end

    def parse_response(raw)
      begin
        resp = JSON.parse(raw)
      rescue JSON::ParserError => e
        raise ProtocolError, "malformed JSON: #{e.message}"
      end

      unless resp.is_a?(Hash)
        raise ProtocolError, "expected JSON object, got #{resp.class}"
      end

      ok     = resp['ok']
      data   = resp['data']
      age_ms = (resp['age_ms'] || 0).to_i
      stale  = resp['stale'] == true
      error  = resp['error']

      unless ok
        raise ServerError, (error || 'unknown error')
      end

      Result.new(ok: ok, data: data, age_ms: age_ms, stale: stale, error: error)
    end
  end

  # Client sends individual requests, opening a fresh socket connection for
  # each call. For workloads that issue many queries per invocation, use
  # {#session} to reuse a persistent connection.
  #
  # Examples:
  #
  #   client = Beachcomber::Client.new
  #   result = client.get('git.branch', path: '/repo')
  #   puts result.data if result.hit?
  class Client
    # @param socket_path [String, nil] explicit socket path; auto-discovered when nil
    # @param timeout [Numeric] connect/read timeout in seconds (default 0.1)
    def initialize(socket_path: nil, timeout: DEFAULT_TIMEOUT)
      @socket_path = socket_path || Discovery.socket_path
      @timeout     = timeout
    end

    # Reads a cached value.
    #
    # @param key [String] e.g. "git.branch" or "git"
    # @param path [String, nil] optional working-directory context
    # @return [Result]
    # @raise [DaemonNotRunning] when the socket cannot be reached
    # @raise [ServerError] when the daemon returns ok: false
    def get(key, path: nil)
      req = { op: 'get', key: key }
      req[:path] = path if path
      roundtrip(req)
    end

    # Forces the daemon to recompute a provider/key.
    #
    # @param key [String]
    # @param path [String, nil]
    # @raise [DaemonNotRunning]
    # @raise [ServerError]
    def refresh(key, path: nil)
      req = { op: 'refresh', key: key }
      req[:path] = path if path
      roundtrip(req)
      nil
    end

    # Lists available providers registered with the daemon.
    #
    # @return [Result]
    def list
      roundtrip({ op: 'list' })
    end

    # Returns scheduler and cache status from the daemon.
    #
    # @return [Result]
    def status
      roundtrip({ op: 'status' })
    end

    # Opens a persistent session and yields it to the block. The connection is
    # closed automatically when the block returns (even on exception).
    #
    # @yield [Session]
    # @return the block's return value
    def session
      sock    = dial
      session = Session.new(sock, @timeout)
      yield session
    ensure
      session&.close
    end

    private

    def roundtrip(req)
      sock = dial
      begin
        s = Session.new(sock, @timeout)
        s.send(:roundtrip, req)
      ensure
        sock.close unless sock.closed?
      end
    end

    def dial
      sock = Socket.new(:UNIX, :STREAM)
      addr = Socket.pack_sockaddr_un(@socket_path)

      # Apply timeout to both connect and subsequent reads/writes.
      sock.setsockopt(Socket::SOL_SOCKET, Socket::SO_SNDTIMEO, timeval(@timeout))
      sock.setsockopt(Socket::SOL_SOCKET, Socket::SO_RCVTIMEO, timeval(@timeout))

      begin
        sock.connect(addr)
      rescue Errno::ENOENT, Errno::ECONNREFUSED, Errno::EACCES
        sock.close
        raise DaemonNotRunning.new(@socket_path)
      end

      sock
    end

    # Packs a Float (seconds) into the C timeval structure expected by setsockopt.
    def timeval(seconds)
      secs  = seconds.to_i
      usecs = ((seconds - secs) * 1_000_000).to_i
      [secs, usecs].pack('l_2')
    end
  end
end
