require 'fiddle'
require 'json'
require 'rbconfig'

module Beachcomber
  # Loads libbeachcomber and exposes the bc_* C ABI as callable
  # Fiddle::Function objects, plus JSON-envelope decoding shared by
  # Client/Session/WatchStream.
  #
  # Discovery is the one rule every SDK follows (docs/sdk-api.md, "Library
  # discovery"; the shared fixture is sdks/discovery.json):
  #
  #   1. $BEACHCOMBER_LIB, when set and not empty. It is then the only
  #      candidate: a path set on purpose that does not load is an error,
  #      not a reason to load some other library.
  #   2. Beside `comb`: <dir>/../lib/<file>, where <dir> is the directory of
  #      the first executable `comb` on $PATH, its symlinks resolved (a
  #      Homebrew `comb` is a symlink into its Cellar, the library beside it).
  #   3. The system loader, given the bare file name.
  #
  # The candidates are tried in order and the first that loads wins. When
  # none does, LibraryNotFoundError names every candidate and why it failed;
  # a missing required symbol is LibrarySymbolError. There is no fallback to
  # a subprocess transport.
  module FFI
    # The library's file name: libbeachcomber.dylib on macOS and
    # libbeachcomber.so on every other platform.
    def self.library_basename(host_os = RbConfig::CONFIG['host_os'])
      host_os =~ /darwin/i ? 'libbeachcomber.dylib' : 'libbeachcomber.so'
    end

    LIB_BASENAME = library_basename

    # One location the loader tries: where it came from, and the path (or,
    # for the system loader, the bare file name) handed to dlopen.
    Candidate = Struct.new(:description, :path)

    # The 22 bc_* symbols this binding calls, checked at load (not on first
    # use) so a version-skewed or partial install fails loudly up front.
    REQUIRED_SYMBOLS = %w[
      bc_version bc_client_new bc_client_free bc_string_free
      bc_get bc_put bc_refresh bc_status bc_introspect bc_hello
      bc_eval
      bc_session_open bc_session_close bc_session_get bc_session_put bc_session_refresh
      bc_session_set_context
      bc_watch_open bc_watch_next bc_watch_cancel bc_watch_free
    ].freeze

    GET_FORCE = 1 << 0
    GET_WAIT  = 1 << 1

    VOIDP = Fiddle::TYPE_VOIDP
    INT   = Fiddle::TYPE_INT
    VOID  = Fiddle::TYPE_VOID

    class << self
      attr_reader :library_path
    end

    # The first executable `comb` on $PATH, its symlinks resolved; nil when
    # there is none.
    def self.resolved_comb_path
      ENV.fetch('PATH', '').split(File::PATH_SEPARATOR).each do |dir|
        next if dir.nil? || dir.empty?

        candidate = File.join(dir, 'comb')
        next unless File.file?(candidate) && File.executable?(candidate)

        return File.realpath(candidate)
      end
      nil
    rescue Errno::ENOENT, Errno::EACCES
      nil
    end

    # The locations to try for +file+, in order. Reads the environment and
    # $PATH; loads nothing.
    #
    # @return [Array<Candidate>]
    def self.candidates(file = LIB_BASENAME)
      explicit = ENV['BEACHCOMBER_LIB']
      return [Candidate.new('$BEACHCOMBER_LIB', explicit)] if explicit && !explicit.empty?

      out = []
      comb = resolved_comb_path
      if comb
        beside = File.expand_path(File.join(File.dirname(comb), '..', 'lib', file))
        out << Candidate.new("beside comb (#{comb})", beside)
      end
      out << Candidate.new('the system loader', file)
    end

    # The first of +candidates+ that loads, as [handle, path]; when none
    # does, LibraryNotFoundError naming each and why it failed.
    def self.open_first(candidates)
      tried = candidates.map do |candidate|
        return [Fiddle.dlopen(candidate.path), candidate.path]
      rescue Fiddle::DLError => e
        "#{candidate.description} (#{candidate.path}): #{e.message}"
      end
      raise Beachcomber::LibraryNotFoundError.new(
        "could not load libbeachcomber; tried, in order: #{tried.join('; ')}", library_version: nil,
      )
    end

    # Loads the library (idempotent) and returns the Fiddle::Handle.
    def self.load!
      return @handle if defined?(@handle) && @handle

      finish_load!(*open_first(candidates))
    end

    def self.finish_load!(handle, path)
      @handle       = handle
      @library_path = path
      check_symbols!
      bind_functions!
      @handle
    end
    private_class_method :finish_load!

    def self.symbol_present?(name)
      @handle[name]
      true
    rescue Fiddle::DLError
      false
    end
    private_class_method :symbol_present?

    def self.check_symbols!
      missing = REQUIRED_SYMBOLS.reject { |sym| symbol_present?(sym) }
      return if missing.empty?

      raise Beachcomber::LibrarySymbolError.new(
        "#{@library_path} is missing required symbol(s): #{missing.join(', ')}",
        library_version: safe_version_for_error,
      )
    end
    private_class_method :check_symbols!

    def self.safe_version_for_error
      return 'unknown' unless symbol_present?('bc_version')

      fn = Fiddle::Function.new(@handle['bc_version'], [], VOIDP)
      ptr = fn.call
      ptr.null? ? 'unknown' : ptr.to_s
    rescue StandardError
      'unknown'
    end
    private_class_method :safe_version_for_error

    def self.bind_functions!
      @fn = {
        bc_version: fnew('bc_version', [], VOIDP),
        bc_client_new: fnew('bc_client_new', [VOIDP], VOIDP),
        bc_client_free: fnew('bc_client_free', [VOIDP], VOID),
        bc_string_free: fnew('bc_string_free', [VOIDP], VOID),
        bc_get: fnew('bc_get', [VOIDP, VOIDP, VOIDP, VOIDP, INT], VOIDP),
        bc_put: fnew('bc_put', [VOIDP, VOIDP, VOIDP, VOIDP, VOIDP], VOIDP),
        bc_refresh: fnew('bc_refresh', [VOIDP, VOIDP, VOIDP], VOIDP),
        bc_status: fnew('bc_status', [VOIDP], VOIDP),
        bc_introspect: fnew('bc_introspect', [VOIDP, VOIDP, VOIDP], VOIDP),
        bc_hello: fnew('bc_hello', [VOIDP], VOIDP),
        bc_eval: fnew('bc_eval', [VOIDP, VOIDP, VOIDP, VOIDP], VOIDP),
        bc_session_open: fnew('bc_session_open', [VOIDP], VOIDP),
        bc_session_close: fnew('bc_session_close', [VOIDP], VOID),
        bc_session_get: fnew('bc_session_get', [VOIDP, VOIDP, VOIDP, VOIDP, INT], VOIDP),
        bc_session_put: fnew('bc_session_put', [VOIDP, VOIDP, VOIDP, VOIDP, VOIDP], VOIDP),
        bc_session_refresh: fnew('bc_session_refresh', [VOIDP, VOIDP, VOIDP], VOIDP),
        bc_session_set_context: fnew('bc_session_set_context', [VOIDP, VOIDP], VOIDP),
        bc_watch_open: fnew('bc_watch_open', [VOIDP, VOIDP, VOIDP, VOIDP], VOIDP),
        bc_watch_next: fnew('bc_watch_next', [VOIDP, INT], VOIDP),
        bc_watch_cancel: fnew('bc_watch_cancel', [VOIDP], VOID),
        bc_watch_free: fnew('bc_watch_free', [VOIDP], VOID),
      }
    end
    private_class_method :bind_functions!

    def self.fnew(name, args, ret)
      Fiddle::Function.new(@handle[name], args, ret)
    end
    private_class_method :fnew

    # The loaded library's build version. Static string; never freed.
    def self.version
      load!
      ptr = @fn[:bc_version].call
      ptr.null? ? '' : ptr.to_s
    end

    # The build version of the library already loaded, or nil when none is:
    # never loads one, so an error raised while loading can ask for it.
    def self.loaded_version
      return nil unless defined?(@fn) && @fn

      version
    end

    # Calls a void*(...)-returning bc_* function, reads the NUL-terminated
    # JSON result, frees it via bc_string_free, and returns the raw JSON
    # string. Not for bc_version, whose result must never be freed.
    def self.raw_call(sym, *args)
      load!
      fn = @fn.fetch(sym) { raise ArgumentError, "unknown bc_* function #{sym}" }
      ptr = fn.call(*args)
      raise Beachcomber::IoError, "NULL returned from #{sym}" if ptr.nil? || ptr.null?

      json = ptr.to_s
      @fn[:bc_string_free].call(ptr)
      json
    end

    # Like raw_call, but decodes the {"ok":...} envelope and raises the
    # idiomatic exception for ok:false, returning only the op's `data` on
    # success. Not for bc_watch_next, whose envelope has a different shape.
    def self.call!(sym, *args)
      envelope = JSON.parse(raw_call(sym, *args))
      unless envelope['ok']
        err = envelope['error'] || {}
        Beachcomber.raise_for_error(err['kind'] || 'server_error', err['message'] || 'unknown error')
      end
      envelope['data']
    end

    # Opaque-handle constructors. These return a raw pointer (BcClient*,
    # BcSession*, BcWatch*), not a JSON envelope — never NULL except
    # BcWatch* on allocation failure.
    def self.new_client(options_json)
      load!
      @fn[:bc_client_new].call(options_json)
    end

    def self.new_session(client_handle)
      load!
      @fn[:bc_session_open].call(client_handle)
    end

    def self.new_watch(client_handle, key, cwd, env_json)
      load!
      @fn[:bc_watch_open].call(client_handle, key, cwd, env_json)
    end

    # Void-returning teardown calls (null-safe on the C side; harmless if
    # called more than once from a Ruby finalizer racing an explicit close).
    def self.free_client(handle)
      load!
      @fn[:bc_client_free].call(handle)
    end

    def self.close_session(handle)
      load!
      @fn[:bc_session_close].call(handle)
    end

    def self.cancel_watch(handle)
      load!
      @fn[:bc_watch_cancel].call(handle)
    end

    def self.free_watch(handle)
      load!
      @fn[:bc_watch_free].call(handle)
    end
  end
end
