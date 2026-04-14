# Non-Goals

Decisions about what beachcomber deliberately does not pursue, and why. These aren't ruled out forever — if circumstances change, they can be revisited. But for now, the project is better served by focusing elsewhere.

---

## Embedded Lua Provider Backend

An embedded Lua interpreter (via `mlua`) was considered as a middle ground between script providers (easy but fork+exec per run) and shared library providers (fast but require a C toolchain).

**Decision:** The shared library backend already covers the "fast without forking" case, and script providers are sufficient for most custom use cases — a few milliseconds on a 30-second poll interval is negligible. Adding an embedded interpreter would meaningfully expand the dependency tree and binary attack surface for a daemon that listens on a socket, without a compelling gap to fill.

## Windows Support / Scoop Package

beachcomber's architecture is built around Unix domain sockets, `libc` calls, and POSIX filesystem semantics. A Windows port would require rethinking the IPC layer, platform-specific provider rewrites, and ongoing CI investment for a platform with a fundamentally different shell ecosystem.

**Decision:** The project focuses on macOS and Linux, where the shell environment caching problem is most acute. A Windows port would be a significant undertaking with different design constraints — better suited as a separate project if there's demand.

## MacPorts Package

beachcomber is available via Homebrew, cargo install, pre-built binaries, Nix, AUR, deb, and rpm.

**Decision:** Each additional package manager adds ongoing maintenance cost (version bumps, checksum updates, reviewer coordination). The current set of install methods covers the vast majority of macOS and Linux users. Additional package managers can be revisited based on user requests.

## Protocol / Config Format Stability Guarantees

Formally freezing the wire protocol or TOML config format was considered as a stability signal.

**Decision:** The protocol and config are not direct user interfaces — they're abstracted behind the CLI and client SDKs, which provide the actual stability contract. Freezing the wire format would constrain future improvements without benefiting users who interact through the supported interfaces. If direct socket consumers emerge as a pattern, this can be revisited.

## Shared Memory / mmap Read Path

A memory-mapped read path would bypass the Unix socket for cache reads, reducing per-query latency from ~15 microseconds to nanoseconds.

**Decision:** 15 microseconds is already invisible in every real consumer context (shell prompts, status bars, editor plugins). Adding a shared memory path would mean maintaining a second serialization format, cross-process synchronization, platform-specific memory management, and a parallel code path in every SDK — substantial ongoing complexity for a performance gain that no workload requires today.
