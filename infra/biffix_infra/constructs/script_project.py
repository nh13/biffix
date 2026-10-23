"""A CodeBuild project that runs one of biffix's build scripts (`codebuild/`), inlined into its
buildspec at synth so what a build runs is fixed by the deploy."""

from __future__ import annotations

import base64
import gzip
from pathlib import Path

from aws_cdk import Duration
from aws_cdk import aws_codebuild as codebuild
from aws_cdk import aws_logs as logs
from constructs import Construct

from biffix_infra.paths import CODEBUILD_DIR

# The shared functions every build script sources.
COMMON_SCRIPT = CODEBUILD_DIR / "lib/common.sh"


def bundled_script(script: Path) -> str:
    """A build script with the shared functions it sources prepended (so it runs self-contained)."""
    return COMMON_SCRIPT.read_text() + "\n" + script.read_text()


# A CodeBuild buildspec, as the JSON object CodeBuild reads.
Buildspec = dict[str, object]


def inline_buildspec(
    script: str, outputs: list[str], runtimes: dict[str, str] | None = None
) -> Buildspec:
    """A buildspec that runs `script`: it travels gzipped + base64 inside the buildspec (no quoting
    to get wrong, and small), and its KEY=VALUE outputs are exported into the shell CodeBuild
    exports `outputs` from. The outputs file is read line by line and each value exported as the
    literal text after the first `=`, never sourced: some values are read out of the images a
    build handles. A key that is not one of `outputs` (or a line with no `=`) fails the build."""
    packed = base64.b64encode(gzip.compress(script.encode(), mtime=0)).decode()
    declared = "|".join(outputs)
    export_outputs = (
        "bad=0; while IFS= read -r line; do "
        '[ -n "$line" ] || continue; key=${line%%=*}; '
        f'case "$key" in {declared}) '
        '[ "$key" != "$line" ] && export "$key=${line#*=}" || bad=1 ;; '
        "*) bad=1 ;; esac; "
        '[ "$bad" = 0 ] || { echo "biffix: unexpected build output line: $line" >&2; break; }; '
        'done < /tmp/biffix-build.env; [ "$bad" = 0 ]'
    )
    phases: dict[str, object] = {
        "build": {
            "commands": [
                f"echo {packed} | base64 -d | gunzip > /tmp/biffix-build.sh",
                "BIFFIX_OUTPUTS=/tmp/biffix-build.env bash /tmp/biffix-build.sh",
                export_outputs,
            ]
        }
    }
    if runtimes:
        phases = {"install": {"runtime-versions": runtimes}, **phases}
    return {
        "version": "0.2",
        "env": {"shell": "bash", "exported-variables": outputs},
        "phases": phases,
    }


class InlineScriptProject(codebuild.Project):
    """CodeBuild project `project_name` running `script` (`bundled_script`, `inline_buildspec`)
    with the plain-text `environment_variables`, on `build_image`, logging to `log_group`. The
    script reports its `outputs` back; `runtimes` are the buildspec's install runtime-versions.
    It has no source and runs outside the VPC."""

    def __init__(
        self,
        scope: Construct,
        cid: str,
        *,
        project_name: str,
        description: str,
        script: Path,
        outputs: list[str],
        build_image: codebuild.IBuildImage,
        compute_type: codebuild.ComputeType,
        environment_variables: dict[str, str],
        timeout: Duration,
        log_group: logs.ILogGroup,
        privileged: bool = False,
        runtimes: dict[str, str] | None = None,
    ) -> None:
        super().__init__(
            scope,
            cid,
            project_name=project_name,
            description=description,
            build_spec=codebuild.BuildSpec.from_object(
                inline_buildspec(bundled_script(script), outputs, runtimes)
            ),
            environment=codebuild.BuildEnvironment(
                build_image=build_image, compute_type=compute_type, privileged=privileged
            ),
            environment_variables={
                name: codebuild.BuildEnvironmentVariable(value=value)
                for name, value in environment_variables.items()
            },
            timeout=timeout,
            logging=codebuild.LoggingOptions(
                cloud_watch=codebuild.CloudWatchLoggingOptions(log_group=log_group)
            ),
        )
