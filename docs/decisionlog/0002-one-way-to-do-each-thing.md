
## Moved from CLAUDE.md (2026-10-20)

<!-- from ws/beachcomber/CLAUDE.md lines 139-166 -->

## One way to do each thing

Pre-1.0. The codebase does what the current design says, directly, and nothing else. When an approach changes, the old one is deleted in the same change — code, tests and docs. No legacy branches, deprecation warnings, compatibility parameters or fallbacks to an older path. Real cruft this rule exists to stop:

- ✗ Two ABIs, chosen at runtime (`src/provider/library.rs`):
  ```rust
  pub enum LibraryAbi {
      /// Phase 4: `bc_source_count / bc_source_metadata / bc_source_execute`.
      MultiSource,
      /// Legacy: `beachcomber_provider_metadata / execute / free`.
      Legacy,
  }
  ```
- ✗ A removed key warned about instead of rejected, plus a test that pins it parsing cleanly (`src/config.rs`, `src/config_tests.rs`):
  ```rust
  "[lifecycle] cache_lifespan is deprecated and ignored; ..."
  name: "legacy_cache_lifespan_parses_without_error",
  ```
- ✗ Bad input silently routed to another path (`src/provider/library.rs`):
  ```rust
  debug!("Library provider '{}': failed to parse metadata JSON, using config fallback", ...);
  ```
- ✗ A parameter kept only so old call sites compile (`src/client.rs`):
  ```rust
  /// `ttl` is accepted for signature compatibility but ... is inert here
  ```
- ✓ One ABI, one config shape, one code path. A removed key is a config error. Bad input is an error the user sees.

