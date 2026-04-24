require "test/unit"
require "socket"
require "tmpdir"
require_relative "../lib/beachcomber/client"

class TestConnectRetry < Test::Unit::TestCase
  def test_connect_retries_succeed_after_brief_outage
    Dir.mktmpdir("comb-retry-") do |dir|
      sock_path = File.join(dir, "sock")

      binder = Thread.new do
        sleep 0.4
        srv = UNIXServer.new(sock_path)
        client = srv.accept
        client&.close
        srv.close
      end

      start = Time.now
      sock = Beachcomber::Client._connect_with_retry(sock_path)
      elapsed = Time.now - start

      assert_not_nil sock
      assert elapsed >= 0.25, "expected retry; elapsed=#{elapsed}"
      sock.close
      binder.join
    end
  end

  def test_connect_retries_exhaust
    Dir.mktmpdir("comb-retry-") do |dir|
      sock_path = File.join(dir, "nosock")
      start = Time.now
      assert_raise(Errno::ECONNREFUSED, Errno::ENOENT) do
        Beachcomber::Client._connect_with_retry(sock_path)
      end
      elapsed = Time.now - start
      assert elapsed >= 1.7, "expected full retry wait; elapsed=#{elapsed}"
    end
  end
end
