# Makefile for libbeachcomber C SDK
#
# Targets:
#   make          — build shared + static libraries
#   make test     — build and run the test suite
#   make install  — install headers and libraries to $(PREFIX)
#   make clean    — remove build artefacts

# ---------------------------------------------------------------------------
# Toolchain
# ---------------------------------------------------------------------------

CC      ?= cc
AR      ?= ar
INSTALL ?= install

CFLAGS  ?= -O2
CFLAGS  += -Wall -Wextra -Wpedantic -std=c99
CFLAGS  += -D_POSIX_C_SOURCE=200809L

# ---------------------------------------------------------------------------
# Platform detection
# ---------------------------------------------------------------------------

UNAME := $(shell uname -s)

ifeq ($(UNAME), Darwin)
    SHARED_LIB  := libbeachcomber.dylib
    SHARED_FLAGS := -dynamiclib -install_name @rpath/$(SHARED_LIB)
else
    SHARED_LIB  := libbeachcomber.so
    SHARED_FLAGS := -shared -Wl,-soname,$(SHARED_LIB)
    # Position-independent code required for shared libs on Linux
    CFLAGS      += -fPIC
endif

STATIC_LIB := libbeachcomber.a

# ---------------------------------------------------------------------------
# Sources
# ---------------------------------------------------------------------------

LIB_SRCS := beachcomber.c json.c
LIB_OBJS := $(LIB_SRCS:.c=.o)

TEST_SRC := test_beachcomber.c
TEST_BIN := test_beachcomber

# ---------------------------------------------------------------------------
# Default target: both libraries
# ---------------------------------------------------------------------------

.PHONY: all
all: $(SHARED_LIB) $(STATIC_LIB)

# ---------------------------------------------------------------------------
# Shared library
# ---------------------------------------------------------------------------

$(SHARED_LIB): $(LIB_OBJS)
	$(CC) $(SHARED_FLAGS) -o $@ $^

# ---------------------------------------------------------------------------
# Static library
# ---------------------------------------------------------------------------

$(STATIC_LIB): $(LIB_OBJS)
	$(AR) rcs $@ $^

# ---------------------------------------------------------------------------
# Object files
# ---------------------------------------------------------------------------

%.o: %.c
	$(CC) $(CFLAGS) -c -o $@ $<

# Explicit dependencies
beachcomber.o: beachcomber.c beachcomber.h json.h
json.o:        json.c json.h

# ---------------------------------------------------------------------------
# Test binary
# ---------------------------------------------------------------------------

# Tests link the library sources directly to avoid needing the shared lib
# in the dynamic linker search path during development.
$(TEST_BIN): $(TEST_SRC) $(LIB_SRCS) beachcomber.h json.h
	$(CC) $(CFLAGS) -o $@ $(TEST_SRC) $(LIB_SRCS) -lpthread

.PHONY: test
test: $(TEST_BIN)
	./$(TEST_BIN)

# ---------------------------------------------------------------------------
# Install
# ---------------------------------------------------------------------------

PREFIX  ?= /usr/local
INCDIR  := $(PREFIX)/include
LIBDIR  := $(PREFIX)/lib

.PHONY: install
install: all
	$(INSTALL) -d $(DESTDIR)$(INCDIR)
	$(INSTALL) -d $(DESTDIR)$(LIBDIR)
	$(INSTALL) -m 644 beachcomber.h $(DESTDIR)$(INCDIR)/beachcomber.h
	$(INSTALL) -m 755 $(SHARED_LIB) $(DESTDIR)$(LIBDIR)/$(SHARED_LIB)
	$(INSTALL) -m 644 $(STATIC_LIB) $(DESTDIR)$(LIBDIR)/$(STATIC_LIB)

# ---------------------------------------------------------------------------
# Clean
# ---------------------------------------------------------------------------

.PHONY: clean
clean:
	rm -f $(LIB_OBJS) $(SHARED_LIB) $(STATIC_LIB) $(TEST_BIN)
