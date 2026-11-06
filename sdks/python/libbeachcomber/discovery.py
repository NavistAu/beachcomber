"""Library discovery for the beachcomber C ABI (``libbeachcomber.{so,dylib}``).

The one rule every SDK follows (docs/sdk-api.md, "Library discovery"; the
shared fixture is ``sdks/discovery.json``):

1. ``$BEACHCOMBER_LIB``, when it is set and not empty. It is then the only
   candidate: a path set on purpose that does not load is an error, not a
   reason to load some other library.
2. Beside ``comb``: ``<dir>/../lib/<file>``, where ``<dir>`` is the
   directory of the first executable ``comb`` on ``$PATH``, its symlinks
   resolved (a Homebrew ``comb`` is a symlink into its Cellar, and the
   library sits there beside it).
3. The system loader, given the bare file name.

The loader tries them in order and stops at the first that loads; when none
does it raises naming every candidate and why each failed. There is no
fallback to a subprocess transport: a missing library is a broken install.
"""

from __future__ import annotations

import os
import shutil
import sys
from dataclasses import dataclass
from typing import List, Optional


def library_basename() -> str:
    """The library's file name: ``libbeachcomber.dylib`` on macOS and
    ``libbeachcomber.so`` on every other platform."""
    return "libbeachcomber.dylib" if sys.platform == "darwin" else "libbeachcomber.so"


@dataclass
class Candidate:
    """One location the loader tries.

    Attributes:
        description: Where the path came from, for the error naming every
            candidate (``"$BEACHCOMBER_LIB"``, ``"beside comb (...)"``,
            ``"the system loader"``).
        path: What is handed to the loader: a path, or the bare file name
            for the system loader.
    """

    description: str
    path: str


def find_comb() -> Optional[str]:
    """The first executable ``comb`` on ``$PATH``, its symlinks resolved;
    ``None`` when there is none."""
    found = shutil.which("comb")
    return os.path.realpath(found) if found else None


def candidates() -> List[Candidate]:
    """The locations to try, in order. Reads the environment and ``$PATH``;
    loads nothing."""
    basename = library_basename()

    explicit = os.environ.get("BEACHCOMBER_LIB")
    if explicit:
        return [Candidate("$BEACHCOMBER_LIB", explicit)]

    out: List[Candidate] = []
    comb = find_comb()
    if comb:
        beside = os.path.normpath(os.path.join(os.path.dirname(comb), "..", "lib", basename))
        out.append(Candidate(f"beside comb ({comb})", beside))
    out.append(Candidate("the system loader", basename))
    return out
