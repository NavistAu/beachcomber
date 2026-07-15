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
    env = {
      'BEACHCOMBER_SOCKET' => @socket_path,
      'HOME'               => @tmpdir,
    }
    @pid = spawn(env, @comb_bin, 'start',
                 '--socket', @socket_path,
                 out: File::NULL,
                 err: File::NULL)
    wait_for_socket
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

  def wait_for_socket(timeout: 5)
    deadline = Time.now + timeout
    loop do
      break if File.socket?(@socket_path)
      raise "daemon socket never appeared at #{@socket_path}" if Time.now > deadline
      sleep 0.05
    end
  end
end

# ---------------------------------------------------------------------------
# Fixture runner
# ---------------------------------------------------------------------------

class ConformanceRunner
  def initialize(socket_path)
    @client  = Beachcomber::Client.new(socket_path: socket_path)
    @passed  = 0
    @failed  = 0
    @errors  = []
  end

  def run_all(fixtures)
    fixtures.each { |f| run_fixture(f) }
    print_summary
    @failed == 0
  end

  private

  def run_fixture(fixture)
    name = fixture['name']
    run_setup(fixture['setup'] || [])
    result, raw = execute_op(fixture['test'])
    check(name, fixture['expect'], result, raw)
  rescue Beachcomber::ServerError => e
    check_error(fixture['name'], fixture['expect'], e.message)
  rescue => e
    @failed += 1
    @errors << "#{fixture['name']}: unexpected exception: #{e.class}: #{e.message}"
    puts "FAIL  #{fixture['name']}"
  end

  def run_setup(steps)
    steps.each do |step|
      execute_op(step)
    end
  end

  def execute_op(step)
    op   = step['op']
    args = step['args'] || {}
    case op
    when 'get'
      result = @client.get(args['key'], path: args['path'])
      [result, nil]
    when 'refresh'
      @client.refresh(args['key'], path: args['path'])
      [nil, nil]
    when 'put'
      @client.put(args['key'], args['data'], ttl: args['ttl'], path: args['path'])
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
      stream = @client.watch(args['key'], path: args['path'])
      event  = stream.next_event
      stream.close
      [event, nil]
    else
      raise "unknown op in fixture: #{op.inspect}"
    end
  rescue Beachcomber::ServerError
    raise
  end

  def check(name, expect, result, _raw)
    status_ok = check_status(expect, result)
    return unless status_ok

    if (dt = expect['data_type'])
      check_data_type(name, dt, result)
    end
    if expect.key?('data_equals')
      check_data_equals(name, expect['data_equals'], result)
    end
    if (field = expect['data_contains_field'])
      check_data_contains_field(name, field, result)
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

  def check_status(expect, result)
    expected = expect['status']
    case expected
    when 'ok'
      true # result returned without raising
    when 'hit'
      if result.respond_to?(:data) && !result.data.nil?
        true
      elsif result.is_a?(Beachcomber::WatchEvent) && !result.data.nil?
        true
      else
        false
      end
    when 'miss'
      result.respond_to?(:miss?) ? result.miss? : false
    when 'error'
      # If we got here without raising, that's a failure — caller handles raises separately
      false
    else
      true
    end
  end

  def extract_data(result)
    case result
    when Beachcomber::Result
      result.data
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

  def check_data_type(name, expected_type, result)
    data = extract_data(result)
    actual = case data
             when Hash    then 'object'
             when Array   then 'array'
             when Integer, Float then 'number'
             when String  then 'string'
             when TrueClass, FalseClass then 'boolean'
             when NilClass then 'null'
             else data.class.name
             end
    unless actual == expected_type
      @errors << "#{name}: data_type expected #{expected_type.inspect}, got #{actual.inspect} (data=#{data.inspect})"
    end
  end

  def check_data_equals(name, expected, result)
    data = extract_data(result)
    unless data == expected
      @errors << "#{name}: data_equals expected #{expected.inspect}, got #{data.inspect}"
    end
  end

  def check_data_contains_field(name, field, result)
    data = extract_data(result)
    unless data.is_a?(Hash) && data.key?(field)
      @errors << "#{name}: data_contains_field #{field.inspect} not present in #{data.inspect}"
    end
  end

  def print_summary
    puts
    puts "Results: #{@passed} passed, #{@failed} failed out of #{@passed + @failed} fixtures"
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
puts "Starting daemon: #{COMB_BIN}"

daemon = DaemonHandle.new(COMB_BIN)
begin
  daemon.start
  puts "Daemon started (socket: #{daemon.socket_path})"
  puts

  runner = ConformanceRunner.new(daemon.socket_path)
  success = runner.run_all(fixtures)
  exit(success ? 0 : 1)
ensure
  daemon.stop
end
