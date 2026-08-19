# Makefile for the beachcomber C SDK
#
# This SDK is a direct binding over libbeachcomber's C ABI (see
# beachcomber.h): there is no protocol code of its own to build anymore
# (Phase 5 of docs/superpowers/plans/2026-08-15-client-abi-and-sdk-refactor.md
# deleted the old beachcomber.c / json.c / json.h). Building anything here
# means linking against libbeachcomber.{dylib,so}, produced by
# `cargo build -p libbeachcomber-ffi` from the workspace root.
#
# Targets:
#   make               — builds the conformance runner (alias for `conformance`)
#   make conformance   — builds sdks/c/conformance_runner
#   make test          — builds and runs the conformance runner (there is no
#                        separate unit-test binary post-collapse: the ABI
#                        itself is exercised by libbeachcomber-ffi's own
#                        Rust tests, and this binding has no logic left to
#                        unit-test independently of those)
#   make install       — installs the header and libbeachcomber.pc to $(PREFIX)
#   make clean         — removes build artefacts

# ---------------------------------------------------------------------------
# Toolchain
# ---------------------------------------------------------------------------

CC      ?= cc
INSTALL ?= install

CFLAGS  ?= -O2
CFLAGS  += -Wall -Wextra -Wpedantic -std=c99
CFLAGS  += -D_POSIX_C_SOURCE=200809L

# No separate ABI version — the shared library, this binding, and `comb`
# itself all carry the workspace version (see Task 3.1 of the plan above).
VERSION ?= 0.7.0

# ---------------------------------------------------------------------------
# libbeachcomber (the cdylib) — same locations libbeachcomber.pc(.in)
# describes for an installed copy, defaulted here to the in-repo dev build
# so `make conformance` works straight out of a workspace checkout with no
# install step. Override BC_LIB_DIR / BC_INCLUDE_DIR to build against an
# installed library instead (matching PREFIX below).
# ---------------------------------------------------------------------------

BC_LIB_DIR     ?= ../../target/debug
BC_INCLUDE_DIR ?= ../../libbeachcomber-ffi/include

UNAME := $(shell uname -s)
ifeq ($(UNAME), Darwin)
    BC_LIB := $(BC_LIB_DIR)/libbeachcomber.dylib
else
    BC_LIB := $(BC_LIB_DIR)/libbeachcomber.so
endif

# -rpath points the linked binary straight at the in-repo build output, so
# the conformance runner resolves the cdylib without needing an install
# step or DYLD_LIBRARY_PATH/LD_LIBRARY_PATH set at run time.
BC_LDFLAGS := -L$(BC_LIB_DIR) -lbeachcomber -Wl,-rpath,$(abspath $(BC_LIB_DIR))

# ---------------------------------------------------------------------------
# Sources
# ---------------------------------------------------------------------------

# runner_json.[ch] is scoped to the conformance runner alone — see
# runner_json.h. It is not part of the SDK: beachcomber.h declares only the
# bc_* ABI, which returns raw JSON envelope strings for the caller's own
# JSON library.
CONFORM_SRC := conformance_runner.c runner_json.c
CONFORM_BIN := conformance_runner

# ---------------------------------------------------------------------------
# Default target
# ---------------------------------------------------------------------------

.PHONY: all
all: conformance

# ---------------------------------------------------------------------------
# libbeachcomber — build it if it isn't already there. Does not attempt to
# track staleness against Rust sources beyond that; `cargo build` after
# editing the FFI crate before re-running `make` if you've changed it.
# ---------------------------------------------------------------------------

$(BC_LIB):
	cd ../.. && cargo build -p libbeachcomber-ffi

# ---------------------------------------------------------------------------
# Conformance runner
# ---------------------------------------------------------------------------

$(CONFORM_BIN): $(CONFORM_SRC) beachcomber.h runner_json.h $(BC_LIB)
	$(CC) $(CFLAGS) -I. -o $@ $(CONFORM_SRC) $(BC_LDFLAGS)

.PHONY: conformance
conformance: $(CONFORM_BIN)

.PHONY: test
test: conformance
	TMPDIR=$${TMPDIR:-/tmp} COMB_BIN=$${COMB_BIN:-../../target/debug/comb} ./$(CONFORM_BIN)

# ---------------------------------------------------------------------------
# Install — header + pkg-config file only. The library itself ships
# alongside `comb` via the project's release packaging (Task 3.10 of the
# plan above), not from this SDK directory.
# ---------------------------------------------------------------------------

PREFIX       ?= /usr/local
INCDIR       := $(PREFIX)/include
LIBDIR       := $(PREFIX)/lib
PKGCONFIGDIR := $(LIBDIR)/pkgconfig

libbeachcomber.pc: libbeachcomber.pc.in
	sed -e 's|@PREFIX@|$(PREFIX)|g' -e 's|@VERSION@|$(VERSION)|g' $< > $@

.PHONY: install
install: libbeachcomber.pc
	$(INSTALL) -d $(DESTDIR)$(INCDIR)
	$(INSTALL) -d $(DESTDIR)$(PKGCONFIGDIR)
	$(INSTALL) -m 644 beachcomber.h $(DESTDIR)$(INCDIR)/beachcomber.h
	$(INSTALL) -m 644 libbeachcomber.pc $(DESTDIR)$(PKGCONFIGDIR)/libbeachcomber.pc

# ---------------------------------------------------------------------------
# Clean
# ---------------------------------------------------------------------------

.PHONY: clean
clean:
	rm -f $(CONFORM_BIN) libbeachcomber.pc
