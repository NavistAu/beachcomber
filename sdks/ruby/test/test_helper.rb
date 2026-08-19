require 'minitest/autorun'
require 'socket'
require 'json'
require 'tmpdir'
require 'thread'

# Hard-kill the suite after 30 seconds — mirrors nextest global-timeout.
SUITE_TIMEOUT_S = 30
Thread.new do
  sleep SUITE_TIMEOUT_S
  $stderr.puts "\n[TIMEOUT] Test suite exceeded #{SUITE_TIMEOUT_S}s — aborting."
  exit!(1)
end

# The binding loads libbeachcomber via $BEACHCOMBER_LIB / ../lib/ relative to
# `comb` on $PATH / the platform default search path (see
# Beachcomber::FFI). Default to the workspace's own debug build so `rake
# test` works out of the box after `cargo build -p libbeachcomber-ffi`,
# without overriding an explicit caller-provided value.
unless ENV['BEACHCOMBER_LIB'] && !ENV['BEACHCOMBER_LIB'].empty?
  repo_root   = File.expand_path('../../..', __dir__)
  default_lib = File.join(repo_root, 'target', 'debug',
                           RbConfig::CONFIG['host_os'] =~ /darwin/i ? 'libbeachcomber.dylib' : 'libbeachcomber.so')
  ENV['BEACHCOMBER_LIB'] = default_lib if File.exist?(default_lib)
end

$LOAD_PATH.unshift File.expand_path('../lib', __dir__)
require 'beachcomber'

# MockServer spins up a UNIXServer in a background thread and records requests.
# Each canned response must be pushed onto the queue before the client call.
#
# Usage:
#
#   server = MockServer.new
#   server.enqueue('{"ok":true,"data":"main","age_ms":10,"stale":false}')
#   client = Beachcomber::Client.new(socket_path: server.path)
#   result = client.get('git.branch')
#   server.stop
class MockServer
  attr_reader :path, :requests

  def initialize
    @dir      = Dir.mktmpdir('beachcomber-test-')
    @path     = File.join(@dir, 'sock')
    @queue    = Queue.new
    @requests = []
    @mutex    = Mutex.new
    @server   = UNIXServer.new(@path)
    @thread   = Thread.new { serve }
    @thread.abort_on_exception = true
  end

  # Push a raw JSON string (without newline) to be sent as the next response.
  def enqueue(response)
    @queue.push(response)
  end

  def stop
    @thread.kill
    @server.close rescue nil
    FileUtils.rm_rf(@dir)
  end

  private

  def serve
    loop do
      client = @server.accept rescue break
      Thread.new(client) { |conn| handle(conn) }
    end
  end

  # The shared client probes the daemon for version skew via a trailing
  # `hello` exchange immediately after a connection's first request/response
  # (Client: once per Client instance; Session: once per connection) — see
  # `probe_skew_after` in libbeachcomber/src/lib.rs. It is not part of what
  # any test is exercising, so it is answered here transparently: neither
  # recorded in #requests nor allowed to consume a queued response meant for
  # the caller's own next op.
  def handle(conn)
    request_index = 0
    first_op = nil
    probe_handled = false

    conn.each_line do |line|
      req = JSON.parse(line.chomp)
      request_index += 1

      if request_index == 1
        first_op = req['op']
      elsif request_index == 2 && !probe_handled && first_op != 'hello' && req['op'] == 'hello'
        probe_handled = true
        conn.write(%({"ok":true,"data":{"protocol_version":"1","daemon_version":"0.0.0-mock"}}\n))
        next
      end

      @mutex.synchronize { @requests << req }

      resp = @queue.pop(true) rescue '{"ok":true}'
      conn.write(resp + "\n")
    end
  rescue StandardError
    # Ignore connection resets during test teardown.
  ensure
    conn.close rescue nil
  end
end
