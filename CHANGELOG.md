# Changelog

All notable changes to beachcomber will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Core daemon with Unix socket server and socket activation
- Concurrent cache with 157ns read latency
- Scheduler with filesystem watching, poll timers, and poke triggers
- Provider execution timeouts (configurable, default 10s)
- Execution deduplication (prevents thundering herd on filesystem bursts)
- Provider failure backoff (exponential delay after 3 consecutive failures)
- Subscription manager with multi-tenant cadence resolution
- Backoff/drain lifecycle (Grace -> SlowPoll -> Frozen -> Evict)
- Graceful shutdown via CancellationToken + SIGINT handling
- Daemon auto-shutdown after configurable idle timeout
- Connection context for implicit path resolution
- Staleness computation in cache responses
- CLI: `comb daemon | get | poke | subscribe | list | status`
- 16 built-in providers: hostname, user, git, battery, load, uptime, network, kubecontext, aws, gcloud, terraform, direnv, python, conda, mise, asdf
- Script provider backend for custom providers via config.toml
- Provider enabled/disabled flag in config
- JSON and text output formats
- ClientSession for persistent connections (15µs/query)
- Comprehensive benchmark suite (cache, protocol, providers, socket, throughput)
