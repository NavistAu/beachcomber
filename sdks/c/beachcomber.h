/*
 * beachcomber.h — C SDK header for the beachcomber daemon.
 *
 * This SDK is a direct binding over libbeachcomber's C ABI. There is no
 * hand-written protocol or JSON-parsing code in this directory anymore
 * (see docs/superpowers/plans/2026-08-15-client-abi-and-sdk-refactor.md,
 * Phase 5) — this header is a thin, in-repo passthrough to the
 * cbindgen-generated ABI header at libbeachcomber-ffi/include/beachcomber.h,
 * so the bc_* surface has exactly one source of truth and cannot drift.
 *
 * Build by linking against libbeachcomber.{dylib,so} (produced by
 * `cargo build -p libbeachcomber-ffi`) and calling the bc_* functions
 * declared in the included header — see its doc comments for the full
 * contract (ownership, NULL-safety, flag bits, envelope shape). The
 * `libbeachcomber.pc.in` template in this directory describes how an
 * installed copy of the library and header are discovered via
 * pkg-config.
 */

#include "../../libbeachcomber-ffi/include/beachcomber.h"
