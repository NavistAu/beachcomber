#!/usr/bin/env ruby
# frozen_string_literal: true
#
# Beachcomber Ruby SDK — protocol conformance runner
#
# Loads tests/conformance/**/*.json fixtures from the repository root, spawns
# a real comb daemon, drives the Ruby client against it, and reports pass/fail.
#
# Usage:
#   COMB_BIN=/path/to/comb ruby sdks/ruby/conformance_runner.rb
#
# The runner creates a temporary socket dir, starts the daemon, runs all
# fixtures, and then stops the daemon.  Exit code is 0 on full pass, 1 on
# any failure.

require 'json'
require 'tmpdir'
require 'fileutils'
require 'open3'

$LOAD_PATH.unshift File.expand_path('lib', __dir__)
require 'beachcomber'

REPO_ROOT     = File.expand_path('../..', __dir__)
FIXTURE_GLOB  = File.join(REPO_ROOT, 'tests', 'conformance', '**', '*.json')
COMB_BIN      = ENV.fetch('COMB_BIN', File.join(REPO_ROOT, 'target', 'debug', 'comb'))

# ---------------------------------------------------------------------------
# Daemon lifecycle
# ---------------------------------------------------------------------------

class DaemonHandle
  attr_reader :socket_path

  def initialize(comb_bin)
    @tmpdir      = Dir.mktmpdir('beachcomber-conformance-')
    @socket_path = File.join(@tmpdir, 'sock')
    @comb_bin    = comb_bin
    @pid         = nil
  end

  def start
    # The daemon writes its managed defaults under $XDG_CONFIG_HOME on start
    # (falling back to $HOME/.config) and its log under $XDG_STATE_HOME: all
    # three point into the run's tempdir.
    env = {
      'BEACHCOMBER_SOCKET' => @socket_path,
      'HOME'               => @tmpdir,
      'XDG_CONFIG_HOME'    => @tmpdir,
      'XDG_STATE_HOME'     => File.join(@tmpdir, '.local', 'state'),
    }
    # The daemon says on `--ready-fd` when it serves its socket.
    ready, signal = IO.pipe
    @pid = spawn(env, @comb_bin, 'daemon',
                 '--socket', @socket_path,
                 '--ready-fd', signal.fileno.to_s,
                 signal => signal,
                 out: File::NULL,
                 err: File::NULL)
    signal.close
    wait_for_ready(ready)
    self
  end

  def stop
    return unless @pid
    begin
      Process.kill('TERM', @pid)
      Process.waitpid(@pid)
    rescue Errno::ESRCH, Errno::ECHILD
      nil
    ensure
      FileUtils.rm_rf(@tmpdir)
      @pid = nil
    end
  end

  private

  # The daemon writes a newline on `ready` once it serves its socket, and
  # closes it unwritten if it exits first.
  def wait_for_ready(ready, timeout: 5)
    served = IO.select([ready], nil, nil, timeout) && ready.gets == "\n"
    raise "daemon did not serve #{@socket_path}" unless served
  ensure
    ready.close
  end
end

# ---------------------------------------------------------------------------
# Fixture runner
# ---------------------------------------------------------------------------

class ConformanceRunner
  # Ops this runner's binding can execute. A fixture using any op outside
  # this set must be skipped, not failed — the binding doesn't implement it.
  SUPPORTED_OPS = %w[hello get refresh put status context watch introspect eval].freeze

  # Ops whose "result" is the value itself (a String/Hash/Array/true/nil),
  # not a typed HelloInfo/etc. wrapper, so status and data extraction
  # special-case them.
  RAW_VALUE_OPS = %w[get eval].freeze

  # Every expectation kind documented in tests/conformance/README.md. A
  # fixture using a key outside this set fails loudly rather than being
  # silently ignored — the whole point of this runner is to catch a fixture
  # asserting something the harness doesn't actually check.
  KNOWN_EXPECT_KEYS = %w[
    status data_type data_equals data_as_text data_contains_field
    data_field_equals error_contains
  ].freeze

  def initialize(comb_bin)
    @comb_bin = comb_bin
    @passed   = 0
    @failed   = 0
    @skipped  = 0
    @errors   = []
  end

  def run_all(fixtures)
    fixtures.each { |f| run_fixture(f) }
    print_summary
    @failed == 0
  end

  private

  def unsupported_op(fixture)
    ops = (fixture['setup'] || []).map { |s| s['op'] }
    ops << fixture['test']['op']
    ops.find { |op| !SUPPORTED_OPS.include?(op) }
  end

  # Each fixture runs against a fresh daemon instance (tests/conformance/
  # README.md's isolation contract) — get fixtures in particular reuse
  # provider keys (e.g. "myappcache") across files, so a shared daemon would
  # leak cache state between them.
  def run_fixture(fixture)
    name = fixture['name']
    if (op = unsupported_op(fixture))
      @skipped += 1
      puts "SKIP  #{name}: unsupported op #{op.inspect}"
      return
    end
    if (unknown = (fixture['expect'].keys - KNOWN_EXPECT_KEYS)) && !unknown.empty?
      @failed += 1
      @errors << "#{name}: fixture uses unknown expectation key(s) #{unknown.inspect} — the runner has no check for them"
      puts "FAIL  #{name}"
      return
    end

    daemon = DaemonHandle.new(@comb_bin)
    daemon.start
    begin
      @client = Beachcomber::Client.new(socket_path: daemon.socket_path, timeout_ms: 5000)
      @default_cwd = File.dirname(daemon.socket_path)

      run_setup(fixture['setup'] || [])
      op = fixture['test']['op']
      result, raw = execute_op(fixture['test'], fixture)
      check(name, fixture['expect'], result, raw, op)
    rescue Beachcomber::CombError => e
      check_error(name, fixture['expect'], e.message)
    rescue => e
      @failed += 1
      @errors << "#{name}: unexpected exception: #{e.class}: #{e.message}"
      puts "FAIL  #{name}"
    ensure
      @client&.close
      daemon.stop
    end
  end

  def run_setup(steps)
    steps.each do |step|
      execute_op(step)
    end
  end

  def execute_op(step, fixture = {})
    op   = step['op']
    args = step['args'] || {}
    case op
    when 'get'
      cwd = fixture['cwd'] || @default_cwd
      value = @client.get(args['key'], cwd: cwd, env: fixture['env'],
                                       force: args['force'] || false, wait: args['wait'])
      [value, nil]
    when 'refresh'
      @client.refresh(args['key'], path: args['path'])
      [nil, nil]
    when 'put'
      @client.put(args['key'], args['data'], ttl: args['ttl'], path: args['path'], persist: args['persist'] == true)
      [nil, nil]
    when 'hello'
      result = @client.hello
      [result, nil]
    when 'status'
      rows = @client.status
      [rows, nil]
    when 'introspect'
      result = @client.introspect(args['subject'], duration_secs: args['duration_secs'])
      [result, nil]
    when 'context'
      # context is session-scoped; not testable here without a session — skip
      [nil, nil]
    when 'watch'
      cwd    = fixture['cwd'] || @default_cwd
      stream = @client.watch(args['key'], cwd: cwd, env: fixture['env'])
      event  = stream.next_event
      stream.close
      [event, nil]
    when 'eval'
      cwd = fixture['cwd'] || @default_cwd
      value = @client.eval(args['template'], cwd: cwd, env: fixture['env'])
      [value, nil]
    else
      raise "unknown op in fixture: #{op.inspect}"
    end
  end

  def check(name, expect, result, _raw, op)
    if (status_failure = check_status(expect, result, op))
      @failed += 1
      @errors << "#{name}: #{status_failure}"
      puts "FAIL  #{name}"
      return
    end

    if (dt = expect['data_type'])
      check_data_type(name, dt, result, op)
    end
    if expect.key?('data_equals')
      check_data_equals(name, expect['data_equals'], result, op)
    end
    if (field = expect['data_contains_field'])
      check_data_contains_field(name, field, result, op)
    end
    if expect.key?('data_field_equals')
      check_data_field_equals(name, expect['data_field_equals'], result, op)
    end
    if expect.key?('data_as_text')
      check_data_as_text(name, expect['data_as_text'], result, op)
    end

    if @errors.any? { |e| e.start_with?(name) }
      @failed += 1
      puts "FAIL  #{name}"
    else
      @passed += 1
      puts "pass  #{name}"
    end
  end

  def check_error(name, expect, message)
    expected_status = expect['status']
    if expected_status == 'error'
      needle = expect['error_contains']
      if needle && !message.include?(needle)
        @failed += 1
        @errors << "#{name}: error message #{message.inspect} does not contain #{needle.inspect}"
        puts "FAIL  #{name}"
      else
        @passed += 1
        puts "pass  #{name}"
      end
    else
      @failed += 1
      @errors << "#{name}: expected status #{expected_status.inspect} but got server error: #{message}"
      puts "FAIL  #{name}"
    end
  end

  # See RAW_VALUE_OPS: `get` and `eval` hand back the value itself, so
  # status/data extraction special-case them rather than trying to duck-type
  # that scalar against the wrapper types below.
  #
  # @return [String, nil] a failure reason, or nil when the status holds
  def check_status(expect, result, op)
    expected = expect['status']
    case expected
    when 'ok', nil
      nil # result returned without raising
    when 'hit'
      hit =
        if RAW_VALUE_OPS.include?(op)
          !result.nil?
        else
          result.respond_to?(:data) && !result.data.nil?
        end
      hit ? nil : "expected status hit but data was absent (#{result.inspect})"
    when 'miss'
      miss =
        if RAW_VALUE_OPS.include?(op)
          result.nil?
        else
          result.respond_to?(:data) && result.data.nil?
        end
      miss ? nil : "expected status miss but got #{result.inspect}"
    when 'error'
      # If we got here without raising, that's a failure — caller handles raises separately
      "expected status error but op succeeded with #{result.inspect}"
    else
      "unknown expect status: #{expected.inspect}"
    end
  end

  def extract_data(result, op)
    return result if RAW_VALUE_OPS.include?(op)

    case result
    when Beachcomber::HelloInfo
      { 'protocol_version' => result.protocol_version, 'daemon_version' => result.daemon_version }
    when Beachcomber::IntrospectResponse
      result.daemon ? daemon_health_to_h(result.daemon) : result.other
    when Beachcomber::WatchEvent
      result.data
    when Array
      result # status — keep as array for data_type check
    else
      result
    end
  end

  def daemon_health_to_h(h)
    {
      'pid'             => h.pid,
      'version'         => h.version,
      'uptime_secs'     => h.uptime_secs,
      'socket_path'     => h.socket_path,
      'config_path'     => h.config_path,
      'requests_total'  => h.requests_total,
      'in_flight'       => h.in_flight,
      'active_watchers' => h.active_watchers,
      'cache_entries'   => h.cache_entries,
    }
  end

  def check_data_type(name, expected_type, result, op)
    data = extract_data(result, op)
    actual = case data
             when Hash    then 'object'
             when Array   then 'array'
             when Integer, Float then 'number'
             when String  then 'string'
             when TrueClass, FalseClass then 'bool'
             when NilClass then 'null'
             else data.class.name
             end
    unless actual == expected_type
      @errors << "#{name}: data_type expected #{expected_type.inspect}, got #{actual.inspect} (data=#{data.inspect})"
    end
  end

  def check_data_equals(name, expected, result, op)
    data = extract_data(result, op)
    unless data == expected
      @errors << "#{name}: data_equals expected #{expected.inspect}, got #{data.inspect}"
    end
  end

  def check_data_contains_field(name, field, result, op)
    data = extract_data(result, op)
    unless data.is_a?(Hash) && data.key?(field)
      @errors << "#{name}: data_contains_field #{field.inspect} not present in #{data.inspect}"
    end
  end

  # { "field": "<name>", "value": <json> } — data.field deep-equals value.
  def check_data_field_equals(name, spec, result, op)
    data = extract_data(result, op)
    field = spec['field']
    expected = spec['value']
    unless data.is_a?(Hash) && data.key?(field) && data[field] == expected
      actual = data.is_a?(Hash) ? data[field] : data
      @errors << "#{name}: data_field_equals failed for #{field.inspect}: expected #{expected.inspect}, got #{actual.inspect}"
    end
  end

  # Mirrors the Rust reference runner's CachedData::as_text(): string ->
  # itself, number/bool -> to_s, null -> absent (compared as ""), object/array
  # -> compact JSON text.
  def as_text(data)
    case data
    when String then data
    when Integer, Float, TrueClass, FalseClass then data.to_s
    when NilClass then nil
    else JSON.generate(data)
    end
  end

  def check_data_as_text(name, expected, result, op)
    data = extract_data(result, op)
    actual = as_text(data) || ''
    unless actual == expected
      @errors << "#{name}: data_as_text expected #{expected.inspect}, got #{actual.inspect}"
    end
  end

  def print_summary
    puts
    puts "Results: #{@passed} passed, #{@failed} failed, #{@skipped} skipped out of #{@passed + @failed + @skipped} fixtures"
    if @errors.any?
      puts
      puts "Failures:"
      @errors.each { |e| puts "  - #{e}" }
    end
  end
end

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

unless File.executable?(COMB_BIN)
  warn "comb binary not found or not executable: #{COMB_BIN}"
  warn "Set COMB_BIN=/path/to/comb or build with: cargo build"
  exit 1
end

fixtures = Dir.glob(FIXTURE_GLOB).sort.map do |path|
  JSON.parse(File.read(path))
end

puts "Loaded #{fixtures.size} conformance fixtures"
puts "comb binary: #{COMB_BIN}"
puts

runner = ConformanceRunner.new(COMB_BIN)
success = runner.run_all(fixtures)
exit(success ? 0 : 1)
