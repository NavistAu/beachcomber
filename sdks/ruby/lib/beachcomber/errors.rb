module Beachcomber
  # The base of every error this SDK raises: one subclass per kind in the
  # SDK API table (docs/sdk-api.md), each named for its kind.
  #
  # +kind+ is the stable, machine-readable slug — the C ABI envelope's
  # +error.kind+, or one this SDK raises itself (+library_not_found+,
  # +library_symbol+, +unsupported+). +message+ is the ABI's message,
  # unchanged. +library_version+ is the loaded library's bc_version(), nil
  # when no library was loaded.
  class CombError < StandardError
    class << self
      # The kind slug this class stands for; nil on the base class, which
      # carries a kind this SDK does not know yet.
      attr_reader :kind
    end

    attr_reader :kind, :library_version

    def initialize(message = nil, kind: self.class.kind, library_version: Beachcomber::FFI.loaded_version)
      super(message)
      @kind = kind
      @library_version = library_version
    end
  end

  # Defines the error class for +kind+, named for it.
  def self.define_error(name, kind)
    klass = Class.new(CombError) { @kind = kind }
    const_set(name, klass)
  end
  private_class_method :define_error

  define_error :DaemonNotRunningError, 'daemon_not_running'
  define_error :ConnectionFailedError, 'connection_failed'
  define_error :IoError, 'io_error'
  define_error :ParseError, 'parse_error'
  define_error :ServerError, 'server_error'
  define_error :TimeoutError, 'timeout'
  define_error :BadFlagsError, 'bad_flags'
  define_error :BusyError, 'busy'
  define_error :PanicError, 'panic'
  define_error :VersionSkewError, 'version_skew'
  define_error :LibraryNotFoundError, 'library_not_found'
  define_error :LibrarySymbolError, 'library_symbol'
  define_error :UnsupportedError, 'unsupported'

  # error.kind slug -> its class.
  KIND_TO_CLASS = constants.map { |c| const_get(c) }
                           .select { |c| c.is_a?(Class) && c < CombError }
                           .to_h { |c| [c.kind, c] }
                           .freeze

  # Raises the error for an envelope's error.kind/error.message. A kind
  # this SDK does not know yet still raises a CombError carrying it.
  def self.raise_for_error(kind, message)
    klass = KIND_TO_CLASS[kind]
    raise klass.new(message) if klass

    raise CombError.new(message, kind: kind)
  end
end
