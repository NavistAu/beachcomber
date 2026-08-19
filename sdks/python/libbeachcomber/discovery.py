"""Library discovery for the beachcomber C ABI (``libbeachcomber.{so,dylib}``).

Discovery order, per the Phase-4 binding contract:

1. ``$BEACHCOMBER_LIB`` — exact path to the shared library.
2. ``../lib/<libname>`` relative to the ``comb`` binary resolved on ``$PATH``
   — library and binary ship together, so the one beside the ``comb`` you
   would actually run is the matching one.
3. The platform default dynamic-linker search path.

Failure is loud: if no candidate loads, the caller raises naming every
location tried, in order. There is no silent fallback to a subprocess
transport — a missing library is a broken install.
"""

from __future__ import annotations

import os
import shutil
import sys
from dataclasses import dataclass
from typing import List, Optional


def library_basename() -> str:
    """Return the platform-appropriate shared library filename."""
    if sys.platform == "darwin":
        return "libbeachcomber.dylib"
    if sys.platform.startswith("linux"):
        return "libbeachcomber.so"
    raise RuntimeError(f"unsupported platform for libbeachcomber: {sys.platform!r}")


@dataclass
class Candidate:
    """One location the discovery order tries, in order.

    Attributes:
        description: Human-readable label for error messages (e.g.
            ``"$BEACHCOMBER_LIB"``).
        path: The concrete path/spec to attempt loading, or ``None`` if this
            candidate could not even be constructed (e.g. ``comb`` not on
            ``$PATH``) — still reported, with ``reason`` explaining why.
        reason: Set when ``path`` is ``None``, explaining why this candidate
            was skipped.
    """

    description: str
    path: Optional[str]
    reason: Optional[str] = None


def candidates() -> List[Candidate]:
    """Build the ordered list of library locations to try.

    Pure and side-effect free (aside from reading the environment and
    ``$PATH``) — does not touch the filesystem or attempt to load anything.
    """
    basename = library_basename()
    out: List[Candidate] = []

    env_path = os.environ.get("BEACHCOMBER_LIB")
    if env_path:
        out.append(Candidate("$BEACHCOMBER_LIB", env_path))
    else:
        out.append(Candidate("$BEACHCOMBER_LIB", None, "not set"))

    comb_path = shutil.which("comb")
    if comb_path:
        comb_dir = os.path.dirname(os.path.abspath(comb_path))
        lib_dir = os.path.normpath(os.path.join(comb_dir, "..", "lib"))
        out.append(
            Candidate(f"../lib/ relative to comb ({comb_path})", os.path.join(lib_dir, basename))
        )
    else:
        out.append(
            Candidate("../lib/ relative to comb on $PATH", None, "comb not found on $PATH")
        )

    out.append(Candidate("platform default search path", basename))

    return out
