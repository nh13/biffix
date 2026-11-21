"""A job's image: its resource or the default one, through the image map, to an image file."""

from __future__ import annotations

import json
from pathlib import Path

import pytest
from snakemake_interface_common.exceptions import WorkflowError

from snakemake_executor_plugin_biffix.images import (
    ImageError,
    ImageMap,
    image_file,
    image_file_name,
    job_image,
    normalize,
)


@pytest.mark.parametrize(
    ("ref", "normalized"),
    [
        ("ubuntu", "docker.io/library/ubuntu:latest"),
        ("ubuntu:24.04", "docker.io/library/ubuntu:24.04"),
        ("biocontainers/fastqc:1", "docker.io/biocontainers/fastqc:1"),
        ("index.docker.io/library/ubuntu", "docker.io/library/ubuntu:latest"),
        ("registry-1.docker.io/a/b:2", "docker.io/a/b:2"),
        ("Quay.io/a/b", "quay.io/a/b:latest"),
        ("localhost/w", "localhost/w:latest"),
        ("localhost:5000/w:1", "localhost:5000/w:1"),
        # A digest pins the image: the tag beside it is dropped, as the AWS Batch executor drops it.
        (" quay.io/a/b:1@sha256:AB ", "quay.io/a/b@sha256:ab"),
        ("b@sha256:ab", "docker.io/library/b@sha256:ab"),
    ],
)
def test_a_reference_is_normalized_as_docker_reads_it(ref: str, normalized: str) -> None:
    assert normalize(ref) == normalized


# biffix-workflow's cases (xtask/src/cases/image_ref.rs, from the Rust ImageRef tests), which the
# backend writes the image map's keys with and nf-biffix's port is held to as well.
IMAGE_REF_CASES = Path(__file__).resolve().parents[3] / "schemas" / "image-ref-cases.json"


def test_every_map_key_biffix_workflow_writes_is_found_for_its_reference() -> None:
    """The backend writes a declared image's map keys in biffix-workflow's spelling, which keeps a
    tag beside a digest: each reference biffix-workflow accepts is found under that key here, as
    the AWS Batch executor finds it (both normalize the key and the reference alike). A reference
    it rejects has no key, and is skipped: this port does not validate."""
    cases = json.loads(IMAGE_REF_CASES.read_text())
    accepted = [(case["input"], case["normalized"]) for case in cases if case["normalized"]]
    assert len(accepted) >= 10 and len(accepted) < len(cases)
    assert [(ref, ImageMap({key: "T"}).resolve(ref)) for ref, key in accepted] == [
        (ref, "T") for ref, _ in accepted
    ]


def test_a_digest_names_the_image_whatever_tag_is_beside_it() -> None:
    """As on AWS Batch: a rule that names an image by digest alone, or with another tag, runs the
    image biffix.yaml declared with a tag and that digest (the keys the backend writes for it)."""
    digest = "sha256:" + "a" * 64
    image_map = ImageMap({f"ghcr.io/org/tool:1.0@{digest}": "T", "ghcr.io/org/tool:1.0": "T"})
    for ref in (f"ghcr.io/org/tool@{digest}", f"ghcr.io/org/tool:2.0@{digest}"):
        assert image_map.resolve(ref) == "T"
    # Two tags of one digest are one image, which the map must run as one.
    with pytest.raises(ValueError, match="map to different images"):
        ImageMap({f"ghcr.io/org/tool:1.0@{digest}": "T", f"ghcr.io/org/tool:2.0@{digest}": "U"})


def test_the_map_answers_to_a_reference_as_written_and_normalized() -> None:
    image_map = ImageMap({"tool:1": "T", "docker.io/library/other:2": "O"})
    assert image_map.resolve("tool:1") == "T"
    assert image_map.resolve("docker.io/library/tool:1") == "T"
    assert image_map.resolve("other:2") == "O"
    assert image_map.resolve("tool:2") is None


def test_the_map_s_own_spelling_wins_over_a_normalized_one() -> None:
    image_map = ImageMap({"ubuntu": "A", "docker.io/library/ubuntu:latest": "B"})
    assert image_map.resolve("ubuntu") == "A"
    assert image_map.resolve("docker.io/library/ubuntu:latest") == "B"
    assert image_map.resolve("index.docker.io/library/ubuntu") == "B"


def test_two_spellings_of_one_image_must_agree() -> None:
    with pytest.raises(ValueError, match="map to different images"):
        ImageMap({"ubuntu": "A", "ubuntu:latest": "B"})
    assert ImageMap({"ubuntu": "A", "ubuntu:latest": "A"}).resolve("ubuntu:latest") == "A"


@pytest.mark.parametrize("entry", [{"a": ""}, {"a": 3}, {"": "b"}, {"a": None}])
def test_a_map_entry_must_be_a_pair_of_references(entry: dict[str, object]) -> None:
    with pytest.raises(ValueError, match="not a pair of references"):
        ImageMap(entry)


def test_a_map_file_must_be_one_json_object_without_repeated_keys(tmp_path: Path) -> None:
    path = tmp_path / "images.json"
    path.write_text(json.dumps({"tool:1": "T"}))
    image_map = ImageMap.from_file(str(path))
    assert image_map.resolve("tool:1") == "T" and image_map.path == str(path)
    path.write_text('{"a": "x", "a": "y"}')
    with pytest.raises(ValueError, match="in the map twice"):
        ImageMap.from_file(str(path))
    path.write_text("[]")
    with pytest.raises(ValueError, match="must be a JSON object"):
        ImageMap.from_file(str(path))


def test_a_job_runs_its_resource_s_image_else_the_default_through_the_map() -> None:
    image_map = ImageMap({"tool:1": "T", "W": "W"})
    assert job_image({"aws_batch_container_image": "tool:1"}, "a", "W", image_map) == "T"
    assert job_image({"mem_mb": 1}, "a", "W", image_map) == "W"
    # No default image: a job that names none runs without a container.
    assert job_image({}, "a", None, image_map) is None
    # Without a map, the image is run as named.
    assert job_image({"aws_batch_container_image": "tool:1"}, "a", "W", None) == "tool:1"
    assert job_image({}, "a", "W", None) == "W"


def test_a_job_whose_image_the_map_lacks_is_refused() -> None:
    image_map = ImageMap({"tool:1": "T"}, path="/run/images.json")
    with pytest.raises(ImageError) as refused:
        job_image({"aws_batch_container_image": "x:1"}, "a", None, image_map)
    assert (refused.value.reason, refused.value.job_name, refused.value.image) == (
        ImageError.NOT_IN_MAP,
        "a",
        "x:1",
    )
    with pytest.raises(WorkflowError, match="a: container image 'x:1' is not in the container"):
        job_image({"aws_batch_container_image": "x:1"}, "a", None, image_map)
    with pytest.raises(WorkflowError, match="image map /run/images.json; add it to the map"):
        job_image({}, "a", "default:1", image_map)
    for image_map_or_none in (image_map, None):
        with pytest.raises(WorkflowError, match="a: container image 3 is not an image reference"):
            job_image({"aws_batch_container_image": 3}, "a", None, image_map_or_none)


@pytest.mark.parametrize("default_image", ["W", None])
@pytest.mark.parametrize("with_map", [True, False])
def test_an_empty_image_resource_is_refused_not_taken_for_none(
    default_image: str | None, with_map: bool
) -> None:
    """A rule that sets its image to "" (an unset config value, say) names no image: the AWS Batch
    executor refuses it (its map's `require`), so the job is refused here too, rather than run in
    the default image or on the host."""
    image_map = ImageMap({"W": "W"}) if with_map else None
    with pytest.raises(WorkflowError, match="a: container image '' is not an image reference"):
        job_image({"aws_batch_container_image": ""}, "a", default_image, image_map)
    with pytest.raises(ImageError) as refused:
        job_image({"aws_batch_container_image": ""}, "a", default_image, image_map)
    assert (refused.value.reason, refused.value.image) == (ImageError.NOT_A_REFERENCE, "")


@pytest.mark.parametrize(
    ("image", "name"),
    [
        ("reg.io/t@sha256:1", "reg.io-t@sha256-1.img"),
        ("123.dkr.ecr.r.amazonaws.com/w:1", "123.dkr.ecr.r.amazonaws.com-w-1.img"),
        ("ubuntu", "ubuntu.img"),
        ("a/b.sif", "a-b.sif"),
        ("a/b.sif:1", "a-b-1.sif"),
    ],
)
def test_an_image_s_file_is_named_as_the_coordinator_names_it(image: str, name: str) -> None:
    assert image_file_name(image) == name


def test_a_job_whose_image_file_is_missing_is_refused(tmp_path: Path) -> None:
    (tmp_path / "reg.io-t@sha256-1.img").write_text("image file")
    assert image_file(tmp_path, "reg.io/t@sha256:1", "a") == tmp_path / "reg.io-t@sha256-1.img"
    with pytest.raises(WorkflowError, match=r"a: the image file of x:1 \(.*x-1.img\) is missing"):
        image_file(tmp_path, "x:1", "a")
    with pytest.raises(ImageError) as refused:
        image_file(tmp_path, "x:1", "a")
    assert (refused.value.reason, refused.value.image, refused.value.path) == (
        ImageError.NOT_CONVERTED,
        "x:1",
        tmp_path / "x-1.img",
    )
