module Beachcomber
  # Base class for all Beachcomber errors.
  class Error < StandardError; end

  # Raised when libbeachcomber cannot be located. Names every location tried,
  # in order, per the shared discovery contract.
  class LibraryNotFound < Error; end

  # Raised when a loaded libbeachcomber is missing a required bc_* symbol.
  # Names the symbol and the bc_version() of what was actually loaded.
  class MissingSymbol < Error; end

  # Base class for every `ok: false` envelope the library returns, plus the
  # library-level conditions (bad_flags, busy, panic, version_skew) it can
  # raise outside the CombError variant set.
  #
  # +kind+ is the stable, machine-readable slug from the envelope's
  # +error.kind+ field (e.g. "server_error", "daemon_not_running"). The
  # message always includes the loaded library's bc_version().
  class CombError < Error
    attr_reader :kind

    def initialize(kind, message)
      @kind = kind
      super("beachcomber: #{message} (kind=#{kind}, lib_version=#{safe_version})")
    end

    private

    def safe_version
      Beachcomber::FFI.version
    rescue StandardError
      'unknown'
    end
  end

  # FFI-boundary conditions (no CombError variant on the Rust side).
  class BadFlagsError < CombError
    def initialize(message)
      super('bad_flags', message)
    end
  end

  class BusyError < CombError
    def initialize(message)
      super('busy', message)
    end
  end

  class PanicError < CombError
    def initialize(message)
      super('panic', message)
    end
  end

  class VersionSkewError < CombError
    def initialize(message)
      super('version_skew', message)
    end
  end

  # One class per `CombError` variant on the Rust side (see
  # libbeachcomber-ffi/src/envelope.rs).
  class DaemonNotRunning < CombError
    def initialize(message)
      super('daemon_not_running', message)
    end
  end

  class ConnectionFailedError < CombError
    def initialize(message)
      super('connection_failed', message)
    end
  end

  class IoError < CombError
    def initialize(message)
      super('io_error', message)
    end
  end

  # Raised when a response cannot be parsed as valid JSON, or the daemon
  # rejects malformed input.
  class ProtocolError < CombError
    def initialize(message)
      super('parse_error', message)
    end
  end

  # Raised when the daemon returns ok: false for an op it actually executed.
  class ServerError < CombError
    def initialize(message)
      super('server_error', message)
    end
  end

  class TimeoutError < CombError
    def initialize(message)
      super('timeout', message)
    end
  end

  # error.kind slug -> exception class. Kept in sync with
  # libbeachcomber-ffi/src/envelope.rs's ErrorKind enum.
  KIND_TO_CLASS = {
    'bad_flags' => BadFlagsError,
    'busy' => BusyError,
    'panic' => PanicError,
    'version_skew' => VersionSkewError,
    'daemon_not_running' => DaemonNotRunning,
    'connection_failed' => ConnectionFailedError,
    'io_error' => IoError,
    'parse_error' => ProtocolError,
    'server_error' => ServerError,
    'timeout' => TimeoutError,
  }.freeze

  # Raises the idiomatic exception for an envelope's error.kind/error.message.
  # An unrecognised kind (future ABI additions) still raises CombError with
  # that kind preserved, rather than failing to raise at all.
  def self.raise_for_error(kind, message)
    klass = KIND_TO_CLASS[kind]
    raise klass.new(message) if klass

    raise CombError.new(kind, message)
  end
end
