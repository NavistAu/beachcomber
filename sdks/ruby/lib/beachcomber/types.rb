module Beachcomber
  # The typed shapes of the SDK API table (docs/sdk-api.md): exactly the
  # fields libbeachcomber::protocol defines, as bc_hello, bc_status and
  # bc_introspect hand them on. A field the ABI leaves out is nil.
  HelloInfo = Struct.new(:protocol_version, :daemon_version, keyword_init: true)

  # One row of #status. +kind+ and +failure+ are the JSON the ABI hands on.
  CacheRow = Struct.new(
    :provider, :path, :source, :field, :value, :age_ms, :stale,
    :kind, :poll_interval_secs, :poll_count, :fsevents_reinstate, :polls_elapsed,
    :next_poll_in_secs, :failure,
    keyword_init: true
  )
  Verdict = Struct.new(:level, :message, keyword_init: true)
  ReaperStatus = Struct.new(:armed, :visibility, :sweeps, :reaped, :kill_denied, keyword_init: true)
  DaemonHealth = Struct.new(
    :pid, :version, :uptime_secs, :socket_path, :config_path,
    :requests_total, :in_flight, :active_watchers, :cache_entries,
    :watch_backend, :reaper, :verdicts,
    keyword_init: true
  )
  # One emitted change on a watch. +data+ is the value — what `comb get`
  # would print for the watched key — or nil when nothing is stored there.
  # A value resolved from several cache records has no single age or
  # staleness, so a watch reports neither.
  WatchEvent = Struct.new(:data, keyword_init: true)

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

  # An #introspect answer. For the DAEMON subject #daemon is a DaemonHealth;
  # for the others #other is the JSON the ABI hands on.
  IntrospectResponse = Struct.new(:subject, :daemon, :other, keyword_init: true)
end
