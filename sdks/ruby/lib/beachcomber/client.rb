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

    # Reads a cached value with protocol flags.
    #
    # @param key [String]
    # @param path [String, nil]
    # @param force [Boolean] bypass cache and recompute
    # @param wait [Boolean] block until a fresh value is available
    # @return [Result]
    def get_with_flags(key, path: nil, force: false, wait: false)
      req = { op: 'get', key: key }
      req[:path]  = path if path
      req[:force] = true if force
      req[:wait]  = true if wait
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

    # Returns daemon status as a raw Result.
    #
    # @return [Result]
    def status
      roundtrip({ op: 'status' })
    end

    # Returns daemon status as an array of CacheRow structs.
    #
    # @return [Array<CacheRow>]
    def status_rows
      resp_obj = roundtrip_raw({ op: 'status' })
      parse_cache_rows(resp_obj)
    end

    # Sends a hello handshake and returns server info.
    #
    # @return [HelloInfo]
    def hello
      resp = roundtrip_raw({ op: 'hello' })
      parse_hello(resp)
    end

    # Writes a value into the daemon cache.
    #
    # @param key [String]
    # @param data [Object, nil]
    # @param ttl [Numeric, nil] time-to-live in seconds
    # @param path [String, nil]
    # @return [nil]
    def put(key, data = nil, ttl: nil, path: nil)
      req = { op: 'put', key: key }
      req[:data] = data unless data.nil?
      req[:ttl]  = ttl  if ttl
      req[:path] = path if path
      roundtrip(req)
      nil
    end

    # Introspects a daemon subsystem.
    #
    # @param subject [String] one of the IntrospectSubject constants
    # @param duration_secs [Numeric, nil]
    # @return [IntrospectResponse]
    def introspect(subject, duration_secs: nil)
      req = { op: 'introspect', subject: subject.to_s }
      req[:duration_secs] = duration_secs if duration_secs
      resp = roundtrip_raw(req)
      parse_introspect(subject.to_s, resp)
    end

    # Closes the underlying socket connection.
    def close
      @socket.close unless @socket.closed?
    end

    private

    def roundtrip(req)
      resp = roundtrip_raw(req)
      build_result(resp)
    end

    def roundtrip_raw(req)
      line = JSON.generate(req) + "\n"
      @socket.write(line)
      raw = @socket.gets
      raise ProtocolError, "connection closed before response" if raw.nil?

      parse_response_hash(raw.chomp)
    end

    def parse_response_hash(raw)
      begin
        resp = JSON.parse(raw)
      rescue JSON::ParserError => e
        raise ProtocolError, "malformed JSON: #{e.message}"
      end

      unless resp.is_a?(Hash)
        raise ProtocolError, "expected JSON object, got #{resp.class}"
      end

      unless resp['ok']
        raise ServerError, (resp['error'] || 'unknown error')
      end

      resp
    end

    def build_result(resp)
      Result.new(
        ok:     resp['ok'],
        data:   resp['data'],
        age_ms: (resp['age_ms'] || 0).to_i,
        stale:  resp['stale'] == true,
        error:  resp['error'],
      )
    end

    def parse_hello(resp)
      data = resp["data"] || {}
      HelloInfo.new(
        protocol_version: data["protocol_version"].to_s,
        daemon_version:   data["daemon_version"].to_s,
      )
    end

    def parse_cache_rows(resp)
      arr = resp["data"]
      raise ProtocolError, "status data is not an array" unless arr.is_a?(Array)
      arr.map do |row|
        CacheRow.new(
          provider:           row["provider"].to_s,
          field:              row["field"],
          path:               row["path"],
          value:              row["value"],
          age_ms:             Integer(row["age_ms"] || 0),
          stale:              row["stale"] == true,
          kind:               row["kind"],
          poll_interval_secs: row["poll_interval_secs"],
          keep_alive_polls:   row["keep_alive_polls"],
          fsevents_reinstate: row["fsevents_reinstate"],
          failure:            row["failure"],
        )
      end
    end

    def parse_daemon_health(data)
      DaemonHealth.new(
        pid:             Integer(data["pid"] || 0),
        version:         data["version"].to_s,
        uptime_secs:     Integer(data["uptime_secs"] || 0),
        socket_path:     data["socket_path"].to_s,
        config_path:     data["config_path"],
        requests_total:  Integer(data["requests_total"] || 0),
        in_flight:       Integer(data["in_flight"] || 0),
        active_watchers: Integer(data["active_watchers"] || 0),
        cache_entries:   Integer(data["cache_entries"] || 0),
        verdicts: (data["verdicts"] || []).map do |v|
          Verdict.new(level: v["level"].to_s, message: v["message"].to_s)
        end,
      )
    end

    def parse_introspect(subject, resp)
      data = resp["data"]
      if subject == IntrospectSubject::DAEMON && data.is_a?(Hash)
        IntrospectResponse.new(subject: subject, daemon: parse_daemon_health(data), other: nil)
      else
        IntrospectResponse.new(subject: subject, daemon: nil, other: data)
      end
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
      with_session { |s| s.send(:roundtrip, req) }
    end

    # Reads a cached value with protocol flags.
    #
    # @param key [String]
    # @param path [String, nil]
    # @param force [Boolean] bypass cache and recompute
    # @param wait [Boolean] block until a fresh value is available
    # @return [Result]
    def get_with_flags(key, path: nil, force: false, wait: false)
      with_session { |s| s.get_with_flags(key, path: path, force: force, wait: wait) }
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
      with_session { |s| s.send(:roundtrip, req) }
      nil
    end

    # Returns daemon status as a raw Result.
    #
    # @return [Result]
    def status
      with_session { |s| s.status }
    end

    # Returns daemon status as an array of CacheRow structs.
    #
    # @return [Array<CacheRow>]
    def status_rows
      with_session { |s| s.status_rows }
    end

    # Sends a hello handshake and returns server info.
    #
    # @return [HelloInfo]
    def hello
      with_session { |s| s.hello }
    end

    # Writes a value into the daemon cache.
    #
    # @param key [String]
    # @param data [Object, nil]
    # @param ttl [Numeric, nil] time-to-live in seconds
    # @param path [String, nil]
    # @return [nil]
    def put(key, data = nil, ttl: nil, path: nil)
      with_session { |s| s.put(key, data, ttl: ttl, path: path) }
    end

    # Introspects a daemon subsystem.
    #
    # @param subject [String] one of the IntrospectSubject constants
    # @param duration_secs [Numeric, nil]
    # @return [IntrospectResponse]
    def introspect(subject, duration_secs: nil)
      with_session { |s| s.introspect(subject, duration_secs: duration_secs) }
    end

    # Opens a persistent watch subscription. Returns a WatchStream (Enumerable).
    # The caller is responsible for closing the stream.
    #
    # @param key [String]
    # @param path [String, nil]
    # @return [WatchStream]
    def watch(key, path: nil)
      sock = open_socket
      req  = { op: 'watch', key: key }
      req[:path] = path if path
      sock.write(JSON.generate(req) + "\n")
      WatchStream.new(sock)
    end

    # Opens a persistent session and yields it to the block. The connection is
    # closed automatically when the block returns (even on exception).
    #
    # @yield [Session]
    # @return the block's return value
    def session
      sock = open_socket
      sess = Session.new(sock, @timeout)
      yield sess
    ensure
      sess&.close
    end

    RETRY_BACKOFFS = [0.250, 0.500, 1.000].freeze

    # Connect to a Unix socket with 3 retries (250ms/500ms/1s exponential).
    # Retries on ECONNREFUSED and ENOENT only — other errors surface immediately.
    # Intended to cover the brief restart window when the daemon is restarting.
    #
    # @param sock_path [String] absolute path to the Unix domain socket
    # @return [UNIXSocket]
    # @raise [Errno::ECONNREFUSED, Errno::ENOENT] after all retries are exhausted
    def self._connect_with_retry(sock_path)
      last_error = nil
      RETRY_BACKOFFS.each do |backoff|
        begin
          return UNIXSocket.new(sock_path)
        rescue Errno::ECONNREFUSED, Errno::ENOENT => e
          last_error = e
          sleep backoff
        end
      end
      # Final attempt — raises if still failing.
      UNIXSocket.new(sock_path)
    end

    private

    def with_session(&block)
      sock = open_socket
      begin
        s = Session.new(sock, @timeout)
        block.call(s)
      ensure
        sock.close unless sock.closed?
      end
    end

    def open_socket
      begin
        sock = self.class._connect_with_retry(@socket_path)
      rescue Errno::ENOENT, Errno::ECONNREFUSED, Errno::EACCES => e
        raise DaemonNotRunning.new(@socket_path)
      end

      # Apply timeouts to the connected socket.
      sock.setsockopt(Socket::SOL_SOCKET, Socket::SO_SNDTIMEO, timeval(@timeout))
      sock.setsockopt(Socket::SOL_SOCKET, Socket::SO_RCVTIMEO, timeval(@timeout))

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
