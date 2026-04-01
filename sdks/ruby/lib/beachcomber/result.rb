module Beachcomber
  # Wraps a daemon response.
  #
  # A result can represent a cache hit (data present), a miss (ok but no data),
  # or an error (ok: false — though errors are normally raised as ServerError).
  #
  # Examples:
  #
  #   result = client.get('git.branch', path: '/repo')
  #   if result.hit?
  #     puts result.data    # "main"
  #     puts result.age_ms  # 42
  #   end
  #
  #   # hash data access
  #   result = client.get('git', path: '/repo')
  #   puts result['branch']
  class Result
    # @return [Boolean] whether the daemon reported success
    attr_reader :ok

    # @return [Object, nil] decoded payload (String, Integer, Hash, Array, etc.)
    attr_reader :data

    # @return [Integer] age of the cached value in milliseconds
    attr_reader :age_ms

    # @return [String, nil] error message when ok is false
    attr_reader :error

    def initialize(ok:, data:, age_ms:, stale:, error:)
      @ok     = ok
      @data   = data
      @age_ms = age_ms
      @stale  = stale
      @error  = error
    end

    # @return [Boolean]
    def ok?
      @ok
    end

    # @return [Boolean] true when the response carried data (cache hit)
    def hit?
      @ok && !@data.nil?
    end

    # @return [Boolean] true when the response was successful but had no data
    def miss?
      @ok && @data.nil?
    end

    # @return [Boolean] true when the cached value is stale
    def stale?
      @stale
    end

    # Delegates field access to the data hash.
    #
    # @param key [String] field name
    # @return [Object, nil]
    # @raise [TypeError] if data is not a Hash
    def [](key)
      unless @data.is_a?(Hash)
        raise TypeError, "Result#[] requires hash data, got #{@data.class}"
      end

      @data[key]
    end

    def inspect
      parts = ["ok=#{@ok}"]
      parts << "data=#{@data.inspect}" unless @data.nil?
      parts << "age_ms=#{@age_ms}" if @age_ms > 0
      parts << "stale=true" if @stale
      parts << "error=#{@error.inspect}" if @error
      "#<Beachcomber::Result #{parts.join(', ')}>"
    end
  end
end
