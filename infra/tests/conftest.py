"""Test-wide setup.

The stacks refuse to synthesize with a missing Rust Lambda artifact, because a placeholder that
exits non-zero once got deployed and silently broke the whole ingest path (see
`biffix_infra.lambda_artifacts`). Tests never deploy, so they opt in to the placeholder explicitly
rather than requiring `cargo lambda build` before `pytest`.
"""

from __future__ import annotations

import os

os.environ.setdefault("BIFFIX_ALLOW_UNBUILT_LAMBDA", "1")
