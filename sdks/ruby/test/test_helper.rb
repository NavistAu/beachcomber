require 'minitest/autorun'
require 'socket'
require 'json'
require 'tmpdir'
require 'thread'

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

  def handle(conn)
    conn.each_line do |line|
      req = JSON.parse(line.chomp)
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
