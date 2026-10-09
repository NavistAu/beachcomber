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

    # The payload carries `template` and `rest` only when set, so a plain
    # hit leaves both nil.
    def build_result(payload)
      Result.new(
        ok: true,
        data: payload['data'],
        age_ms: (payload['age_ms'] || 0).to_i,
        stale: payload['stale'] == true,
        error: nil,
        template: payload['template'],
        rest: payload['rest'],
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
        poll_count: row['poll_count'],
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

    # Reads a node raw — both halves, as {Client#get} describes.
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
    # +data = nil+ removes the node.
    #
    # @param key [String]
    # @param data [Object, nil]
    # @param ttl [Numeric, String, nil] time-to-live
    # @param path [String, nil]
    # @return [nil]
    def put(key, data, ttl: nil, path: nil)
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
    # @param autostart [Boolean, nil] attempt to start the daemon if it isn't
    #   running. When nil (the default) the shared library's default applies
    #   (autostart on); pass false to disable. The library only autostarts
    #   when the socket path is auto-discovered — never for an explicit
    #   socket_path.
    # @param exec [Boolean] whether a template this client resolves
    #   ({#resolve}, {#eval_expression}) may call +exec()+. Off by default:
    #   a template that calls it then fails with
    #   "exec() is disabled in this context".
    def initialize(socket_path: nil, timeout: DEFAULT_TIMEOUT, autostart: nil, exec: false)
      options = {}
      options[:autostart] = autostart unless autostart.nil?
      options[:socket_path] = socket_path if socket_path
      options[:timeout_ms] = (timeout * 1000).round if timeout
      options[:exec] = true if exec

      @handle = Beachcomber::FFI.new_client(JSON.generate(options))
      ObjectSpace.define_finalizer(self, self.class.finalizer(@handle))
    end

    def self.finalizer(handle)
      proc { Beachcomber::FFI.free_client(handle) }
    end

    # Reads a node from the daemon, raw — both halves, nothing rendered.
    #
    # The Result carries the cached value (+data+) and the template in
    # effect at the address (+template+), whichever exist. A template-only
    # result is an address whose value {#resolve} renders — this is the raw
    # half; <tt>resolve(key, cwd:, env:)</tt> is how you get the value.
    #
    # @param key [String] e.g. "git.branch" or "git"
    # @param path [String, nil] optional working-directory context
    # @return [Result] hit? when a cached value exists, miss? when neither a
    #   value nor a template does
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

    # Writes a value into the daemon cache. +data = nil+ removes the node.
    #
    # @param key [String]
    # @param data [Object, nil]
    # @param ttl [Numeric, String, nil] time-to-live (e.g. "60s")
    # @param path [String, nil]
    # @return [nil]
    def put(key, data, ttl: nil, path: nil)
      Beachcomber::FFI.call!(:bc_put, @handle, key, JSON.generate(data), ttl&.to_s, path)
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

    # Resolves +key+ to the same value `comb get` returns for this key, cwd
    # and env. The daemon holds the templates (`<provider>:template`, seeded
    # from configuration or {#put}); this client renders them. A plain field
    # is its cached value, a templated one is rendered, and a bare provider
    # name is the whole merged namespace.
    #
    # @param key [String] "provider.field" or a bare provider name
    # @param cwd [String] required — the library never reads the process's
    #   own cwd on the caller's behalf.
    # @param env [Hash, nil] the caller's environment for `env.*` refs
    # @return [Object, nil] the resolved value, or nil on a miss
    def resolve(key, cwd:, env: nil)
      Beachcomber::FFI.call!(:bc_resolve, @handle, key, cwd, json_or_nil(env))
    end

    # Renders a template that is stored nowhere — the same value `comb eval`
    # prints. Every ref resolves exactly as it would inside a stored
    # template, so <tt>eval_expression('{{ p.f }}')</tt> and
    # <tt>resolve('p.f')</tt> agree.
    #
    # +template+ accepts a single <tt>{{ expr }}</tt> tag, or literal
    # text/several tags — the former keeps the expression's natural type,
    # the latter is always a string (a source with no tags at all is its
    # literal text).
    #
    # @param template [String]
    # @param cwd [String] required, matching {#resolve}
    # @param env [Hash, nil]
    # @return [Object]
    def eval_expression(template, cwd:, env: nil)
      Beachcomber::FFI.call!(:bc_eval, @handle, template, cwd, json_or_nil(env))
    end

    # Opens a persistent watch on the *value* of +key+ — what {#resolve}
    # answers for it — and emits it again whenever it changes. A template
    # edited under the watch fires it just as a changed cache record does.
    # Returns a WatchStream (Enumerable). The caller is responsible for
    # closing the stream.
    #
    # @param key [String] "provider.field" or a bare provider name
    # @param cwd [String] required, matching {#resolve} — the directory
    #   every subscription addresses and every re-resolution runs at
    # @param env [Hash, nil] the caller's environment for `env.*` refs
    # @return [WatchStream]
    def watch(key, cwd:, env: nil)
      handle = Beachcomber::FFI.new_watch(@handle, key, cwd, json_or_nil(env))
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
