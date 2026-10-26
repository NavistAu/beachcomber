require 'json'

require_relative 'ffi'
require_relative 'errors'
require_relative 'types'
require_relative 'watch_stream'

module Beachcomber
  # Request-building helpers shared by Client and Session.
  module Requests
    private

    # The +options_json+ +bc_get+ and +bc_session_get+ take: the options the
    # caller gave, +force+ when it is set and +wait+ as given; none is nil.
    def get_options(force, wait)
      options = {}
      options['force'] = true if force
      options['wait'] = wait unless wait.nil?
      json_or_nil(options.empty? ? nil : options)
    end

    def json_or_nil(value)
      value ? JSON.generate(value) : nil
    end
  end

  # The typed shapes, built from the JSON the ABI hands on: each Struct's
  # members, a member the ABI left out nil.
  module Typed
    module_function

    def of(struct, json)
      struct.new(**struct.members.to_h { |m| [m, json[m.to_s]] })
    end

    def cache_rows(data)
      raise ParseError, 'status data is not an array' unless data.is_a?(Array)

      data.map { |row| of(CacheRow, row) }
    end

    def daemon_health(data)
      health = of(DaemonHealth, data)
      health.reaper = data['reaper'] && of(ReaperStatus, data['reaper'])
      health.verdicts = (data['verdicts'] || []).map { |v| of(Verdict, v) }
      health
    end

    def introspect(subject, data)
      if subject == IntrospectSubject::DAEMON && data.is_a?(Hash)
        IntrospectResponse.new(subject: subject, daemon: daemon_health(data), other: nil)
      else
        IntrospectResponse.new(subject: subject, daemon: nil, other: data)
      end
    end
  end

  # Session holds a persistent connection to the daemon, held open by the
  # shared library, and a directory of its own ({#set_context}) that {#get},
  # {#put} and {#refresh} read or write at when given no directory.
  #
  # Obtain a Session via {Client#session}:
  #
  #   client.session do |s|
  #     s.set_context('/repo')
  #     branch = s.get('git.branch')
  #   end
  #
  # #status, #hello and #introspect address no directory and need no
  # connection of their own, so they are the Client's alone.
  #
  # The underlying handle is guarded by the library's own mutex: a
  # concurrent caller on the same session gets {Beachcomber::BusyError}
  # rather than blocking or interleaving requests.
  class Session
    include Requests

    def initialize(handle)
      @handle = handle
      @closed = false
    end

    # Sets this session's directory: every later {#get} that names no +cwd+,
    # and {#put} and {#refresh} that name no path, address it. The session
    # keeps it and sends it with each request.
    #
    # @param path [String]
    def set_context(path)
      Beachcomber::FFI.call!(:bc_session_set_context, handle!, path)
      nil
    end

    # {Client#get} on this session's persistent connection. +cwd: nil+ is
    # this session's directory ({#set_context}) when it has one, else the
    # global entry.
    #
    # @param key [String] e.g. "git.branch" or "git"
    # @param cwd [String, nil]
    # @param env [Hash, nil] the caller's environment for `env.*` refs
    # @param force [Boolean]
    # @param wait [String, nil] a duration or "forever", as {Client#get}
    # @return [Object, nil] the value, or nil on a miss
    def get(key, cwd: nil, env: nil, force: false, wait: nil)
      Beachcomber::FFI.call!(:bc_session_get, handle!, key, cwd, json_or_nil(env), get_options(force, wait))
    end

    # {Client#refresh} on this session's connection; +path: nil+ is this
    # session's directory.
    #
    # @param key [String]
    # @param path [String, nil]
    def refresh(key, path: nil)
      Beachcomber::FFI.call!(:bc_session_refresh, handle!, key, path)
      nil
    end

    # {Client#put} on this session's connection; +path: nil+ is this
    # session's directory.
    #
    # @param key [String]
    # @param data [Object, nil]
    # @param ttl [Numeric, String, nil] time-to-live
    # @param path [String, nil]
    # @return [nil]
    def put(key, data, ttl: nil, path: nil)
      Beachcomber::FFI.call!(:bc_session_put, handle!, key, JSON.generate(data), ttl&.to_s, path)
      nil
    end

    # Closes the underlying connection. Idempotent.
    def close
      return if @closed

      @closed = true
      Beachcomber::FFI.close_session(@handle)
      @handle = nil
    end

    private

    # The handle every operation passes on. A closed session has none, and
    # the library must never see a freed handle, so the call fails here.
    def handle!
      @handle or raise IoError, 'the session is closed'
    end
  end

  # Client sends individual requests through the shared library, which owns
  # socket handling, framing and JSON mapping. For workloads that issue many
  # queries per invocation, use {#session} to reuse a persistent connection.
  # It follows the SDK API table, docs/sdk-api.md.
  #
  # Examples:
  #
  #   client = Beachcomber::Client.new
  #   branch = client.get('git.branch', cwd: '/repo') # nil on a miss
  #   client.get('git:template.branch')               # the template, unrendered
  #   client.close
  class Client
    include Requests

    # Each option given is passed to +bc_client_new+ as it is; nil is not
    # given, and the library's default applies. This SDK has no defaults of
    # its own.
    #
    # @param socket_path [String, nil] explicit socket path; nil is the
    #   library's discovery.
    # @param timeout_ms [Integer, nil] how long one read or write on the
    #   socket may take, in milliseconds; at least 1. nil is the library's
    #   default (100).
    # @param connect_timeout_ms [Integer, nil] how long connecting keeps
    #   retrying before it gives up, in milliseconds; 0 is one attempt. nil
    #   is the library's default (1500).
    # @param autostart [Boolean, nil] attempt to start the daemon if it isn't
    #   running; nil is the library's default (on). The library only
    #   autostarts when the socket path is auto-discovered — never for an
    #   explicit socket_path.
    # @param exec [Boolean, nil] whether a template this client renders
    #   ({#get}, {#eval}) may call +exec()+; nil is the library's default
    #   (off), and a template that calls it then fails with
    #   "exec() is disabled in this context".
    def initialize(socket_path: nil, timeout_ms: nil, connect_timeout_ms: nil, autostart: nil, exec: nil)
      options = self.class.options_json(
        socket_path: socket_path, timeout_ms: timeout_ms, connect_timeout_ms: connect_timeout_ms,
        autostart: autostart, exec: exec,
      )
      @handle = Beachcomber::FFI.new_client(options)
      ObjectSpace.define_finalizer(self, self.class.finalizer(@handle))
    end

    # The +options_json+ +bc_client_new+ takes: exactly the options given
    # (nil is not given), under the ABI's own names.
    #
    # @return [String]
    def self.options_json(**options)
      JSON.generate(options.compact)
    end

    def self.finalizer(handle)
      proc { Beachcomber::FFI.free_client(handle) }
    end

    # The value `comb get` prints for this key, cwd and env. The daemon holds
    # the templates (`<provider>:template`, seeded from configuration or
    # {#put}); this client renders them. A plain field is its cached value,
    # a templated one is rendered, and a bare provider name is the whole
    # merged namespace. The unrendered halves are facets, read like any key:
    # <tt>"p:template.x"</tt> is the template as stored, <tt>"p:put.x"</tt>
    # and <tt>"p:source.x"</tt> those layers' values.
    #
    # @param key [String] "provider.field" or a bare provider name
    # @param cwd [String, nil] the directory the read addresses (`~` is home,
    #   a relative path is against this process's working directory); nil is
    #   the global entry
    # @param env [Hash, nil] the caller's environment for `env.*` refs; nil
    #   is the calling process's environment, as `comb` renders
    # @param force [Boolean] re-execute every Source the read demands first
    # @param wait [String, nil] how long to wait for a run the read needs
    #   before answering what is stored, as `comb get --wait`: a duration
    #   ("0", "50ms", "2s") or "forever"; nil is the read's budget. The read
    #   may take +timeout_ms+ plus the wait; "forever" has no deadline
    # @return [Object, nil] the value, or nil on a miss
    # @raise [Beachcomber::DaemonNotRunningError] when no daemon answers
    # @raise [Beachcomber::ServerError] when the daemon refuses the read
    # @raise [Beachcomber::ParseError] when +wait+ is no duration
    def get(key, cwd: nil, env: nil, force: false, wait: nil)
      Beachcomber::FFI.call!(:bc_get, handle!, key, cwd, json_or_nil(env), get_options(force, wait))
    end

    # Renders a template that is stored nowhere — the same value `comb eval`
    # prints. Every ref resolves exactly as it would inside a stored
    # template, so <tt>eval('{{ p.f }}')</tt> and <tt>get('p.f')</tt> agree.
    #
    # +template+ accepts a single <tt>{{ expr }}</tt> tag, or literal
    # text/several tags — the former keeps the expression's natural type,
    # the latter is always a string (a source with no tags at all is its
    # literal text).
    #
    # @param template [String]
    # @param cwd [String, nil] read as {#get} reads it; nil is the global entry
    # @param env [Hash, nil] the caller's environment for `env.*` refs
    # @return [Object, nil] the value, or nil when it renders nothing
    def eval(template, cwd: nil, env: nil)
      Beachcomber::FFI.call!(:bc_eval, handle!, template, cwd, json_or_nil(env))
    end

    # Opens a persistent watch on the *value* of +key+ — what {#get}
    # answers for it — and emits it again whenever it changes. A template
    # edited under the watch fires it just as a changed cache record does.
    # Returns a WatchStream (Enumerable). The caller is responsible for
    # closing the stream.
    #
    # @param key [String] "provider.field" or a bare provider name
    # @param cwd [String, nil] read as {#get} reads it — the directory every
    #   subscription addresses and every re-resolution runs at; nil is the
    #   global entry
    # @param env [Hash, nil] the caller's environment for `env.*` refs
    # @return [WatchStream]
    def watch(key, cwd: nil, env: nil)
      handle = Beachcomber::FFI.new_watch(handle!, key, cwd, json_or_nil(env))
      raise Beachcomber::IoError, 'NULL returned from bc_watch_open (allocation failure)' if handle.nil? || handle.null?

      WatchStream.new(handle)
    end

    # Writes a value into the daemon cache. +data = nil+ removes the node.
    #
    # @param key [String]
    # @param data [Object, nil]
    # @param ttl [Numeric, String, nil] time-to-live (e.g. "60s")
    # @param path [String, nil]
    # @return [nil]
    def put(key, data, ttl: nil, path: nil)
      Beachcomber::FFI.call!(:bc_put, handle!, key, JSON.generate(data), ttl&.to_s, path)
      nil
    end

    # Forces the daemon to recompute a provider/key.
    #
    # @param key [String]
    # @param path [String, nil]
    def refresh(key, path: nil)
      Beachcomber::FFI.call!(:bc_refresh, handle!, key, path)
      nil
    end

    # Returns cache rows from the daemon.
    #
    # @return [Array<CacheRow>]
    # @raise [Beachcomber::ParseError] when the answer is not a list of rows
    def status
      Typed.cache_rows(Beachcomber::FFI.call!(:bc_status, handle!))
    end

    # Sends a hello handshake and returns server info.
    #
    # @return [HelloInfo]
    def hello
      Typed.of(HelloInfo, Beachcomber::FFI.call!(:bc_hello, handle!))
    end

    # Introspects a daemon subsystem.
    #
    # @param subject [String] one of the IntrospectSubject constants
    # @param duration_secs [Numeric, nil]
    # @return [IntrospectResponse]
    def introspect(subject, duration_secs: nil)
      options_json = duration_secs ? JSON.generate(duration_secs: duration_secs) : nil
      data = Beachcomber::FFI.call!(:bc_introspect, handle!, subject.to_s, options_json)
      Typed.introspect(subject.to_s, data)
    end

    # Opens a persistent session and yields it to the block. The connection
    # is closed automatically when the block returns (even on exception).
    #
    # @yield [Session]
    # @return the block's return value
    def session
      sess = Session.new(Beachcomber::FFI.new_session(handle!))
      yield sess
    ensure
      sess&.close
    end

    # Frees the client's handle. Idempotent; a later operation fails. The
    # finalizer frees it when a client is never closed.
    def close
      return if @handle.nil?

      ObjectSpace.undefine_finalizer(self)
      Beachcomber::FFI.free_client(@handle)
      @handle = nil
    end

    private

    # The handle every operation passes on. A closed client has none, and
    # the library must never see a NULL or freed handle, so the call fails
    # here, as an io_error.
    def handle!
      @handle or raise IoError, 'the client is closed'
    end
  end
end
