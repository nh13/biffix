"""Fake executables the coordinator's tests put on PATH."""

from __future__ import annotations

import shlex
import sys
from pathlib import Path

# The name a fake was run as. The fake itself is a script beside it, run by the interpreter, so
# its own sys.argv[0] is that script's path, not this.
ARGV0 = "BIFFIX_TEST_ARGV0"


def python_executable(path: Path, source: str) -> None:
    """Make `path` an executable that runs the Python program `source` with this interpreter.

    Through /bin/sh rather than a `#!<interpreter>` line: the kernel splits a shebang at its first
    space, so an interpreter under a path with a space in it would not start. The name it was run
    as (through a symlink, say) is in the environment as `ARGV0`.
    """
    script = path.with_name(f".{path.name}.py")
    script.write_text(source)
    path.write_text(
        "#!/bin/sh\n"
        f'{ARGV0}="$0" exec {shlex.quote(sys.executable)} {shlex.quote(str(script))} "$@"\n'
    )
    path.chmod(0o755)
