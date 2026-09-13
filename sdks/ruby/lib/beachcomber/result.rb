module Beachcomber
  # Wraps a daemon response.
  #
  # `get` returns the node as the daemon holds it — both halves, nothing
  # rendered. A node may hold a cached value (+data+), a template
  # (+template+), or both. A result is a hit when data is present, a miss
  # when neither half is, or an error (ok: false — though errors are normally
  # raised as ServerError). A template-only result is neither a hit nor a
  # miss: it is an address whose value {Client#resolve} renders — this is the
  # raw half; <tt>resolve(key, cwd:, env:)</tt> is how you get the value.
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
  #
  #   result = client.get('scanline.widgets.foo')
  #   result.template  # "{{ env.SCANLINE_FOO }}" — render it with resolve
  class Result
    # @return [Boolean] whether the daemon reported success
    attr_reader :ok

    # @return [Object, nil] decoded payload (String, Integer, Hash, Array, etc.)
    attr_reader :data

    # @return [Integer] age of the cached value in milliseconds
    attr_reader :age_ms

    # @return [String, nil] error message when ok is false
    attr_reader :error

    # @return [String, Hash, nil] the template in effect at the address,
    #   unrendered: a String for a leaf, a Hash for a group of templates;
    #   nil when none is in effect
    attr_reader :template

    # @return [String, nil] the address left over when the template walk
    #   stopped at a string — <tt>get('p.t.x')</tt> where +p.t+ is a
    #   template string gives the template of +p.t+ and rest +"x"+
    attr_reader :rest

    def initialize(ok:, data:, age_ms:, stale:, error:, template: nil, rest: nil)
      @ok       = ok
      @data     = data
      @age_ms   = age_ms
      @stale    = stale
      @error    = error
      @template = template
      @rest     = rest
    end

    # @return [Boolean]
    def ok?
      @ok
    end

    # @return [Boolean] true when the response carried data (cache hit)
    def hit?
      @ok && !@data.nil?
    end

    # @return [Boolean] true when nothing is stored at the address: no data
    #   and no template. A template-only result is not a miss.
    def miss?
      @ok && @data.nil? && @template.nil?
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
      parts << "template=#{@template.inspect}" unless @template.nil?
      parts << "rest=#{@rest.inspect}" unless @rest.nil?
      parts << "error=#{@error.inspect}" if @error
      "#<Beachcomber::Result #{parts.join(', ')}>"
    end
  end
end
