"""Every ARN the stacks build follows the deploy's partition (`Stack.format_arn` or the
`AWS::Partition` reference), never a literal `arn:aws:`, which another partition would deny or fail
to match without an error."""

import dataclasses
import json
import re
from pathlib import Path

from aws_cdk.assertions import Template

from tests.synth.synth_helpers import CONFIG, _build, _single_stacks


def test_no_template_hardcodes_the_aws_partition(tmp_path: Path) -> None:
    # Locked-down worker egress builds the endpoint policies too.
    stacks = _build(tmp_path, config=dataclasses.replace(CONFIG, worker_egress="ecr_s3_only"))
    every = list(_single_stacks(stacks).values())
    hardcoded = {
        stack.stack_name: sorted(set(re.findall(r"arn:aws:[^\"]*", json.dumps(t))))
        for stack in [*every, *stacks["projects"]]
        if (t := Template.from_stack(stack).to_json())
    }
    assert {name: arns for name, arns in hardcoded.items() if arns} == {}
