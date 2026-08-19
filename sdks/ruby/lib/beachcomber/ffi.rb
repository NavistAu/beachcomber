require 'fiddle'
require 'json'
require 'rbconfig'

module Beachcomber
  # Loads libbeachcomber and exposes the bc_* C ABI as callable
  # Fiddle::Function objects, plus JSON-envelope decoding shared by
  # Client/Session/WatchStream.
  #
  # Discovery order (the shared contract every dynamic-language binding
  # follows):
  #
  #   1. $BEACHCOMBER_LIB
  #   2. ../lib/<libname> relative to the resolved `comb` on $PATH
  #   3. the platform default dynamic-linker search path
  #
  # `../lib/` beside `comb` is checked before the system path deliberately:
  # library and binary ship together, so the copy next to the `comb` you
  # would actually run is the matching one, and a stale system-wide copy
  # must not win.
  #
  # A failure to find or load the library, or a missing required symbol, is
  # a loud error naming every location tried (or the missing symbol) plus
  # the loaded library's bc_version() where known. There is no silent
  # fallback to a subprocess transport.
  module FFI
    LIB_BASENAME =
      case RbConfig::CONFIG['host_os']
      when /darwin/i
        'libbeachcomber.dylib'
      when /linux/i
        'libbeachcomber.so'
      else
        raise Beachcomber::Error, "unsupported platform: #{RbConfig::CONFIG['host_os']}"
      end

    # The 22 bc_* symbols this binding calls, checked at load (not on first
    # use) so a version-skewed or partial install fails loudly up front.
    REQUIRED_SYMBOLS = %w[
      bc_version bc_client_new bc_client_free bc_string_free
      bc_get bc_put bc_put_null bc_refresh bc_status bc_introspect bc_hello
      bc_resolve bc_eval
      bc_session_open bc_session_close bc_session_get bc_session_put bc_session_set_context
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

    # Finds `comb` on $PATH the way a shell would, resolving symlinks (a
    # Homebrew-linked binary, for instance) so `../lib/` is computed
    # relative to where the binary actually lives.
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

    def self.candidate_beside_comb
      comb = resolved_comb_path
      return nil unless comb

      File.expand_path(File.join(File.dirname(comb), '..', 'lib', LIB_BASENAME))
    end

    # Loads the library (idempotent) and returns the Fiddle::Handle.
    def self.load!
      return @handle if defined?(@handle) && @handle

      tried = []

      env_lib = ENV['BEACHCOMBER_LIB']
      if env_lib && !env_lib.empty?
        tried << env_lib
        handle = try_open(env_lib)
        return finish_load!(handle, env_lib) if handle
      end

      candidate = candidate_beside_comb
      if candidate
        tried << candidate
        handle = try_open(candidate)
        return finish_load!(handle, candidate) if handle
      end

      tried << "#{LIB_BASENAME} (platform default search path)"
      handle = try_open(LIB_BASENAME)
      return finish_load!(handle, LIB_BASENAME) if handle

      raise Beachcomber::LibraryNotFound,
            "could not locate #{LIB_BASENAME}; tried: #{tried.join(', ')}"
    end

    def self.try_open(path)
      Fiddle.dlopen(path)
    rescue Fiddle::DLError
      nil
    end
    private_class_method :try_open

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

      raise Beachcomber::MissingSymbol,
            "#{@library_path} is missing required symbol(s): #{missing.join(', ')} " \
            "(bc_version=#{safe_version_for_error})"
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
        bc_get: fnew('bc_get', [VOIDP, VOIDP, VOIDP, INT], VOIDP),
        bc_put: fnew('bc_put', [VOIDP, VOIDP, VOIDP, VOIDP, VOIDP], VOIDP),
        bc_put_null: fnew('bc_put_null', [VOIDP, VOIDP, VOIDP], VOIDP),
        bc_refresh: fnew('bc_refresh', [VOIDP, VOIDP, VOIDP], VOIDP),
        bc_status: fnew('bc_status', [VOIDP], VOIDP),
        bc_introspect: fnew('bc_introspect', [VOIDP, VOIDP, VOIDP], VOIDP),
        bc_hello: fnew('bc_hello', [VOIDP], VOIDP),
        bc_resolve: fnew('bc_resolve', [VOIDP, VOIDP, VOIDP, VOIDP, VOIDP], VOIDP),
        bc_eval: fnew('bc_eval', [VOIDP, VOIDP, VOIDP, VOIDP, VOIDP], VOIDP),
        bc_session_open: fnew('bc_session_open', [VOIDP], VOIDP),
        bc_session_close: fnew('bc_session_close', [VOIDP], VOID),
        bc_session_get: fnew('bc_session_get', [VOIDP, VOIDP, VOIDP, INT], VOIDP),
        bc_session_put: fnew('bc_session_put', [VOIDP, VOIDP, VOIDP, VOIDP, VOIDP], VOIDP),
        bc_session_set_context: fnew('bc_session_set_context', [VOIDP, VOIDP], VOIDP),
        bc_watch_open: fnew('bc_watch_open', [VOIDP, VOIDP, VOIDP], VOIDP),
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

    # Calls a void*(...)-returning bc_* function, reads the NUL-terminated
    # JSON result, frees it via bc_string_free, and returns the raw JSON
    # string. Not for bc_version, whose result must never be freed.
    def self.raw_call(sym, *args)
      load!
      fn = @fn.fetch(sym) { raise ArgumentError, "unknown bc_* function #{sym}" }
      ptr = fn.call(*args)
      raise Beachcomber::Error, "unexpected NULL pointer from #{sym}" if ptr.nil? || ptr.null?

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

    def self.new_watch(client_handle, key, path)
      load!
      @fn[:bc_watch_open].call(client_handle, key, path)
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
