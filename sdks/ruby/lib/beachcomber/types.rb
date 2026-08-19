module Beachcomber
  HelloInfo = Struct.new(:protocol_version, :daemon_version, keyword_init: true)
  CacheRow = Struct.new(
    :provider, :field, :path, :value, :age_ms, :stale,
    :kind, :poll_interval_secs, :keep_alive_polls, :fsevents_reinstate, :polls_elapsed,
    :failure, :source,
    keyword_init: true
  )
  Verdict = Struct.new(:level, :message, keyword_init: true)
  Reaper = Struct.new(:armed, :visibility, :sweeps, :reaped, :kill_denied, keyword_init: true)
  DaemonHealth = Struct.new(
    :pid, :version, :uptime_secs, :socket_path, :config_path,
    :requests_total, :in_flight, :active_watchers, :cache_entries,
    :watch_backend, :reaper, :verdicts,
    keyword_init: true
  )
  WatchEvent = Struct.new(:data, :age_ms, :stale, keyword_init: true)

  module IntrospectSubject
    DAEMON    = "daemon"
    PROVIDERS = "providers"
    CONFIG    = "config"
    CACHE     = "cache"
    LIFECYCLE = "lifecycle"
    WATCHES   = "watches"
    TIMERS    = "timers"
    DEMAND    = "demand"
    PROCS     = "procs"
  end

  # Introspect response wrapper. For DAEMON subject: #daemon is populated.
  # For others: #other holds the raw Hash/Array.
  IntrospectResponse = Struct.new(:subject, :daemon, :other, keyword_init: true)
end
