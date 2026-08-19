require 'json'

require_relative 'ffi'
require_relative 'errors'
require_relative 'result'
require_relative 'types'
require_relative 'watch_stream'

module Beachcomber
  DEFAULT_TIMEOUT = 0.1 # seconds (100 ms) — matches libbeachcomber's own default.

  # Response-shaping helpers shared by Client and Session: the ABI's JSON
  # payload shapes are identical regardless of which handle made the call.
  module ResponseParsing
    private

    def build_result(payload)
      Result.new(
        ok: true,
        data: payload['data'],
        age_ms: (payload['age_ms'] || 0).to_i,
        stale: payload['stale'] == true,
        error: nil,
      )
    end

    def parse_hello(data)
      HelloInfo.new(
        protocol_version: data['protocol_version'].to_s,
        daemon_version: data['daemon_version'].to_s,
      )
    end

    def parse_cache_rows(data)
      raise ProtocolError, 'status data is not an array' unless data.is_a?(Array)

      data.map { |row| build_cache_row(row) }
    end

    def build_cache_row(row)
      CacheRow.new(
        provider: row['provider'].to_s,
        field: row['field'],
        path: row['path'],
        value: row['value'],
        age_ms: Integer(row['age_ms'] || 0),
        stale: row['stale'] == true,
        kind: row['kind'],
        poll_interval_secs: row['poll_interval_secs'],
        keep_alive_polls: row['keep_alive_polls'],
        fsevents_reinstate: row['fsevents_reinstate'],
        polls_elapsed: row['polls_elapsed'],
        failure: row['failure'],
        source: row['source'],
      )
    end

    def parse_daemon_health(data)
      reaper = data['reaper']
      DaemonHealth.new(
        pid: Integer(data['pid'] || 0),
        version: data['version'].to_s,
        uptime_secs: Integer(data['uptime_secs'] || 0),
        socket_path: data['socket_path'].to_s,
        config_path: data['config_path'],
        requests_total: Integer(data['requests_total'] || 0),
        in_flight: Integer(data['in_flight'] || 0),
        active_watchers: Integer(data['active_watchers'] || 0),
        cache_entries: Integer(data['cache_entries'] || 0),
        watch_backend: data['watch_backend'],
        reaper: reaper && Reaper.new(
          armed: reaper['armed'],
          visibility: reaper['visibility'],
          sweeps: reaper['sweeps'],
          reaped: reaper['reaped'],
          kill_denied: reaper['kill_denied'],
        ),
        verdicts: (data['verdicts'] || []).map do |v|
          Verdict.new(level: v['level'].to_s, message: v['message'].to_s)
        end,
      )
    end

    def parse_introspect(subject, data)
      if subject == IntrospectSubject::DAEMON && data.is_a?(Hash)
        IntrospectResponse.new(subject: subject, daemon: parse_daemon_health(data), other: nil)
      else
        IntrospectResponse.new(subject: subject, daemon: nil, other: data)
      end
    end

    def json_or_nil(value)
      value ? JSON.generate(value) : nil
    end
  end

  # Session holds a persistent connection to the daemon, held open by the
  # shared library, for {#get}/{#get_with_flags}/{#put}/{#set_context}.
  #
  # Obtain a Session via {Client#session}:
  #
  #   client.session do |s|
  #     s.set_context('/repo')
  #     r = s.get('git.branch')
  #   end
  #
  # {#refresh}, {#status}, {#hello} and {#introspect} are also available for
  # API-compatibility with the pre-ABI client, but the C ABI provides no
  # session-scoped equivalents for them (only get/put/set_context reuse the
  # persistent connection — see Task 3.6 of the client-ABI plan); they route
  # through the parent Client's connection instead, one-shot per call.
  #
  # The underlying handle is guarded by the library's own mutex: a
  # concurrent caller on the same session gets {Beachcomber::BusyError}
  # rather than blocking or interleaving requests.
  class Session
    include ResponseParsing

    def initialize(handle, client_handle)
      @handle = handle
      @client_handle = client_handle
      @closed = false
    end

    # Sets the default path for subsequent {#get}/{#get_with_flags} queries
    # on this connection.
    #
    # @param path [String]
    def set_context(path)
      Beachcomber::FFI.call!(:bc_session_set_context, @handle, path)
      nil
    end

    # Reads a cached value.
    #
    # @param key [String] e.g. "git.branch" or "git"
    # @param path [String, nil] optional working-directory override
    # @return [Result]
    def get(key, path: nil)
      build_result(Beachcomber::FFI.call!(:bc_session_get, @handle, key, path, 0))
    end

    # Reads a cached value with protocol flags.
    #
    # @param key [String]
    # @param path [String, nil]
    # @param force [Boolean] bypass cache and recompute
    # @param wait [Boolean] block until a fresh value is available
    # @return [Result]
    def get_with_flags(key, path: nil, force: false, wait: false)
      flags = (force ? Beachcomber::FFI::GET_FORCE : 0) | (wait ? Beachcomber::FFI::GET_WAIT : 0)
      build_result(Beachcomber::FFI.call!(:bc_session_get, @handle, key, path, flags))
    end

    # Forces the daemon to recompute a provider/key.
    #
    # @param key [String]
    # @param path [String, nil]
    def refresh(key, path: nil)
      Beachcomber::FFI.call!(:bc_refresh, @client_handle, key, path)
      nil
    end

    # Returns cache rows from the daemon.
    #
    # @return [Array<CacheRow>]
    def status
      parse_cache_rows(Beachcomber::FFI.call!(:bc_status, @client_handle))
    end

    # Sends a hello handshake and returns server info.
    #
    # @return [HelloInfo]
    def hello
      parse_hello(Beachcomber::FFI.call!(:bc_hello, @client_handle))
    end

    # Writes a value into the daemon cache on this session's connection.
    # +data = nil+ clears the entry.
    #
    # @param key [String]
    # @param data [Object, nil]
    # @param ttl [Numeric, String, nil] time-to-live
    # @param path [String, nil]
    # @return [nil]
    def put(key, data = nil, ttl: nil, path: nil)
      Beachcomber::FFI.call!(:bc_session_put, @handle, key, JSON.generate(data), ttl&.to_s, path)
      nil
    end

    # Introspects a daemon subsystem.
    #
    # @param subject [String] one of the IntrospectSubject constants
    # @param duration_secs [Numeric, nil]
    # @return [IntrospectResponse]
    def introspect(subject, duration_secs: nil)
      options_json = duration_secs ? JSON.generate(duration_secs: duration_secs) : nil
      data = Beachcomber::FFI.call!(:bc_introspect, @client_handle, subject.to_s, options_json)
      parse_introspect(subject.to_s, data)
    end

    # Closes the underlying connection. Idempotent.
    def close
      return if @closed

      @closed = true
      Beachcomber::FFI.close_session(@handle)
    end
  end

  # Client sends individual requests through the shared library, which owns
  # socket handling, framing and JSON mapping. For workloads that issue many
  # queries per invocation, use {#session} to reuse a persistent connection.
  #
  # Examples:
  #
  #   client = Beachcomber::Client.new
  #   result = client.get('git.branch', path: '/repo')
  #   puts result.data if result.hit?
  class Client
    include ResponseParsing

    # @param socket_path [String, nil] explicit socket path; library default
    #   discovery applies when nil.
    # @param timeout [Numeric, nil] socket read/write timeout in seconds
    #   (default 0.1 / 100ms, matching the library's own default).
    # @param autostart [Boolean] attempt to start the daemon if it isn't
    #   running. Default false, matching this binding's pre-ABI behaviour;
    #   pass true to opt into the library's autostart capability.
    def initialize(socket_path: nil, timeout: DEFAULT_TIMEOUT, autostart: false)
      options = { autostart: autostart }
      options[:socket_path] = socket_path if socket_path
      options[:timeout_ms] = (timeout * 1000).round if timeout

      @handle = Beachcomber::FFI.new_client(JSON.generate(options))
      ObjectSpace.define_finalizer(self, self.class.finalizer(@handle))
    end

    def self.finalizer(handle)
      proc { Beachcomber::FFI.free_client(handle) }
    end

    # Reads a cached value.
    #
    # @param key [String] e.g. "git.branch" or "git"
    # @param path [String, nil] optional working-directory context
    # @return [Result]
    # @raise [Beachcomber::DaemonNotRunning] when the socket cannot be reached
    # @raise [Beachcomber::ServerError] when the daemon returns ok: false
    def get(key, path: nil)
      build_result(Beachcomber::FFI.call!(:bc_get, @handle, key, path, 0))
    end

    # Reads a cached value with protocol flags.
    #
    # @param key [String]
    # @param path [String, nil]
    # @param force [Boolean] bypass cache and recompute
    # @param wait [Boolean] block until a fresh value is available
    # @return [Result]
    def get_with_flags(key, path: nil, force: false, wait: false)
      flags = (force ? Beachcomber::FFI::GET_FORCE : 0) | (wait ? Beachcomber::FFI::GET_WAIT : 0)
      build_result(Beachcomber::FFI.call!(:bc_get, @handle, key, path, flags))
    end

    # Forces the daemon to recompute a provider/key.
    #
    # @param key [String]
    # @param path [String, nil]
    def refresh(key, path: nil)
      Beachcomber::FFI.call!(:bc_refresh, @handle, key, path)
      nil
    end

    # Returns cache rows from the daemon.
    #
    # @return [Array<CacheRow>]
    def status
      parse_cache_rows(Beachcomber::FFI.call!(:bc_status, @handle))
    end

    # Sends a hello handshake and returns server info.
    #
    # @return [HelloInfo]
    def hello
      parse_hello(Beachcomber::FFI.call!(:bc_hello, @handle))
    end

    # Writes a value into the daemon cache. +data = nil+ clears the entry
    # without dropping the registry entry.
    #
    # @param key [String]
    # @param data [Object, nil]
    # @param ttl [Numeric, String, nil] time-to-live (e.g. "60s")
    # @param path [String, nil]
    # @return [nil]
    def put(key, data = nil, ttl: nil, path: nil)
      if data.nil?
        Beachcomber::FFI.call!(:bc_put_null, @handle, key, path)
      else
        Beachcomber::FFI.call!(:bc_put, @handle, key, JSON.generate(data), ttl&.to_s, path)
      end
      nil
    end

    # Introspects a daemon subsystem.
    #
    # @param subject [String] one of the IntrospectSubject constants
    # @param duration_secs [Numeric, nil]
    # @return [IntrospectResponse]
    def introspect(subject, duration_secs: nil)
      options_json = duration_secs ? JSON.generate(duration_secs: duration_secs) : nil
      data = Beachcomber::FFI.call!(:bc_introspect, @handle, subject.to_s, options_json)
      parse_introspect(subject.to_s, data)
    end

    # Resolves a virtual field ("provider.field") or a provider's path
    # expression ("provider") client-side, exactly as `comb get`'s
    # resolution layer does. `cache.*` refs the expression makes are fetched
    # live through this client.
    #
    # @param key [String] "provider.field" or a bare provider name
    # @param cwd [String] required — path-expression evaluation has no
    #   ambient fallback; the library never reads the process's own cwd.
    # @param env [Hash, nil] env var values `env.*` refs resolve against
    # @param overrides [Hash, nil] expression overrides, keyed
    #   "provider.field" or a bare provider name
    # @return [Object, nil] the resolved value, or nil on a path-expression miss
    def resolve(key, cwd:, env: nil, overrides: nil)
      Beachcomber::FFI.call!(:bc_resolve, @handle, key, cwd, json_or_nil(env), json_or_nil(overrides))
    end

    # Evaluates an arbitrary expression string against `env.*`/`cache.*`
    # refs, using the same evaluator {#resolve} uses for a declared field.
    #
    # @param template [String]
    # @param cwd [String] required, matching {#resolve}
    # @param env [Hash, nil]
    # @param overrides [Hash, nil]
    # @return [String]
    def eval_expression(template, cwd:, env: nil, overrides: nil)
      Beachcomber::FFI.call!(:bc_eval, @handle, template, cwd, json_or_nil(env), json_or_nil(overrides))
    end

    # Opens a persistent watch subscription. Returns a WatchStream
    # (Enumerable). The caller is responsible for closing the stream.
    #
    # @param key [String]
    # @param path [String, nil]
    # @return [WatchStream]
    def watch(key, path: nil)
      handle = Beachcomber::FFI.new_watch(@handle, key, path)
      raise Beachcomber::Error, 'bc_watch_open returned NULL (allocation failure)' if handle.nil? || handle.null?

      WatchStream.new(handle)
    end

    # Opens a persistent session and yields it to the block. The connection
    # is closed automatically when the block returns (even on exception).
    #
    # @yield [Session]
    # @return the block's return value
    def session
      handle = Beachcomber::FFI.new_session(@handle)
      sess = Session.new(handle, @handle)
      yield sess
    ensure
      sess&.close
    end
  end
end
