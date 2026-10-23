"""The image restore plan: which archived blobs an image needs, and which must be restored first;
and `main` against a stand-in archive (`aws`) and registry (`crane`). That the keep-tags match the
import's is checked against common.sh itself in test_codebuild_scripts."""

import json
import subprocess
import types
from pathlib import Path

import pytest

from biffix_infra import image_restore
from biffix_infra.config import ARCHIVE_BLOBS_PREFIX
from biffix_infra.image_restore import BlobState, blob_key, blobs_of, keep_tags, plan_restore

INDEX = {"manifests": [{"digest": "sha256:idx"}]}


def _blobs() -> dict[str, bytes]:
    def manifest(layer: str) -> bytes:
        return json.dumps(
            {
                "schemaVersion": 2,
                "config": {"digest": f"sha256:cfg-{layer}"},
                "layers": [{"digest": f"sha256:{layer}"}, {"digest": "sha256:shared"}],
            }
        ).encode()

    return {
        "sha256:idx": json.dumps(
            {
                "schemaVersion": 2,
                "manifests": [{"digest": "sha256:amd64"}, {"digest": "sha256:arm64"}],
            }
        ).encode(),
        "sha256:amd64": manifest("l-amd64"),
        "sha256:arm64": manifest("l-arm64"),
        "sha256:cfg-l-amd64": b'{"architecture": "amd64"}',
        "sha256:cfg-l-arm64": b'{"architecture": "arm64"}',
        "sha256:l-amd64": b"\x1f\x8b binary layer",
        "sha256:l-arm64": b"\x1f\x8b binary layer",
        "sha256:shared": b"",
    }


def test_an_index_needs_every_platform_its_configs_and_layers_once() -> None:
    blobs = _blobs()
    got = blobs_of(INDEX, lambda d: blobs[d])
    assert set(got) == set(blobs)
    assert len(got) == len(set(got)), "a shared layer is listed once"
    assert got[0] == "sha256:idx"


def test_cold_blobs_are_restored_before_anything_is_pushed() -> None:
    plan = plan_restore(
        {
            "sha256:a": BlobState("STANDARD", None),
            "sha256:b": BlobState("GLACIER_IR", None),  # readable at once
            "sha256:c": BlobState("DEEP_ARCHIVE", None),
            "sha256:d": BlobState("DEEP_ARCHIVE", 'ongoing-request="true"'),
            "sha256:e": BlobState("DEEP_ARCHIVE", 'ongoing-request="false", expiry-date="x"'),
        }
    )
    assert plan.request == ["sha256:c"]
    assert plan.waiting == ["sha256:d"]
    assert not plan.ready
    assert plan_restore({"sha256:e": BlobState("DEEP_ARCHIVE", 'ongoing-request="false"')}).ready


def test_blobs_live_under_the_import_layout() -> None:
    assert blob_key("sha256:abc") == "blobs/sha256/abc"
    assert blob_key("sha256:abc").startswith(ARCHIVE_BLOBS_PREFIX)


def test_a_restored_cold_blob_is_readable_and_an_unrestored_one_is_not() -> None:
    assert BlobState("STANDARD", None).readable
    assert BlobState("GLACIER_IR", None).readable
    assert not BlobState("DEEP_ARCHIVE", None).readable
    assert not BlobState("DEEP_ARCHIVE", 'ongoing-request="true"').readable
    # A restored object keeps its storage class; only the Restore header says it is readable.
    assert BlobState("DEEP_ARCHIVE", 'ongoing-request="false", expiry-date="x"').readable
    assert BlobState.of({"StorageClass": "GLACIER", "Restore": 'ongoing-request="false"'}).readable
    assert BlobState.of({}).storage_class == "STANDARD"


def test_a_tiered_manifest_is_discovered_once_restored() -> None:
    """With a low tiering threshold even manifests go cold. An unreadable blob hides what it lists,
    so each rerun (once the previous round of restores finished) discovers one more level."""
    blobs = _blobs()
    restored: set[str] = set()

    def read(digest: str) -> bytes:
        return blobs[digest] if digest in restored else b""

    rounds = 0
    while True:
        found = blobs_of(INDEX, read)
        cold = [d for d in found if d not in restored]
        if not cold:
            break
        restored.update(cold)  # the run requests them; they finish before the next run
        rounds += 1
    assert set(found) == set(blobs)
    assert rounds == 3  # the index; its platform manifests; their configs and layers


def test_restore_keep_tags_number_platforms_as_the_import_does() -> None:
    index = {
        "manifests": [
            {"digest": "sha256:amd", "platform": {"os": "linux", "architecture": "amd64"}},
            {"digest": "sha256:att", "platform": {"os": "unknown", "architecture": "unknown"}},
            {"digest": "sha256:arm", "platform": {"os": "linux", "architecture": "arm64"}},
            {"digest": "sha256:nop"},
        ]
    }
    digest = "sha256:" + "ab" * 32
    assert keep_tags(digest, index) == [
        (digest, "biffix-keep-abababababab"),
        ("sha256:amd", "biffix-keep-abababababab-1"),
        ("sha256:arm", "biffix-keep-abababababab-2"),
    ]
    # A single-platform image is keep-tagged alone.
    assert keep_tags(digest, {"schemaVersion": 2, "layers": []}) == [
        (digest, "biffix-keep-abababababab")
    ]


# ── main, against a stand-in archive and registry ──

DIGEST = "sha256:" + "ab" * 32
REPO = "reg.example/wf"


class _World:
    """The archive bucket (objects and their heads) and the registry's tags, as `main` sees them
    through `_aws` and `crane`."""

    def __init__(self, cold: bool = False, pushes_as: str = DIGEST):
        manifest = {
            "schemaVersion": 2,
            "config": {"digest": "sha256:cfg"},
            "layers": [{"digest": "sha256:layer"}],
        }
        self.objects = {
            f"images/{DIGEST}/index.json": json.dumps({"manifests": [{"digest": DIGEST}]}),
            f"images/{DIGEST}/oci-layout": '{"imageLayoutVersion": "1.0.0"}',
            blob_key(DIGEST): json.dumps(manifest),
            blob_key("sha256:cfg"): '{"architecture": "amd64"}',
            blob_key("sha256:layer"): "\x1f\x8b layer",
        }
        self.heads: dict[str, dict[str, object]] = {
            key: {"ContentLength": len(body)} for key, body in self.objects.items()
        }
        if cold:
            self.heads[blob_key("sha256:layer")]["StorageClass"] = "DEEP_ARCHIVE"
        self.pushes_as = pushes_as
        self.bucket = "arch-bucket"
        self.tags: dict[str, str] = {}
        self.calls: list[tuple[str, ...]] = []

    def aws(self, *args: str) -> str:
        self.calls.append(args)
        if args[:2] == ("cloudformation", "list-stack-resources"):
            return "arch-bucket\n"
        if args[:2] == ("s3", "cp"):
            bucket, _, key = args[3].removeprefix("s3://").partition("/")
            assert bucket == self.bucket, args
            Path(args[4]).write_text(self.objects[key])
            return ""
        if args[:2] == ("s3api", "head-object"):
            return json.dumps(self.heads[args[args.index("--key") + 1]])
        if args[:2] == ("s3api", "restore-object"):
            return ""
        raise AssertionError(args)

    def run(self, cmd: list[str], **_kw: object) -> subprocess.CompletedProcess[str]:
        self.calls.append(tuple(cmd))
        assert cmd[0] == "crane", cmd
        out, rc = "", 0
        if cmd[1] == "digest":
            out, rc = (self.tags[cmd[2]], 0) if cmd[2] in self.tags else ("", 1)
        elif cmd[1] == "push":
            self.tags[cmd[3]] = self.pushes_as
        elif cmd[1] == "tag":
            self.tags[f"{cmd[2].split('@')[0]}:{cmd[3]}"] = cmd[2].split("@")[1]
        return subprocess.CompletedProcess(cmd, rc, out, "")

    def main(self, monkeypatch: pytest.MonkeyPatch, *extra: str) -> int:
        monkeypatch.setattr(image_restore, "_aws", self.aws)
        monkeypatch.setattr(image_restore, "subprocess", types.SimpleNamespace(run=self.run))
        return image_restore.main(["--tenant", "acme", "--digest", DIGEST, "--repo", REPO, *extra])

    def crane(self, verb: str) -> list[tuple[str, ...]]:
        return [c for c in self.calls if c[:2] == ("crane", verb)]


def test_main_restores_a_cold_blob_first_and_pushes_nothing(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    world = _World(cold=True)
    assert world.main(monkeypatch) == 2
    restores = [c for c in world.calls if c[:2] == ("s3api", "restore-object")]
    assert [c[c.index("--key") + 1] for c in restores] == [blob_key("sha256:layer")]
    assert not world.crane("push") and not world.crane("tag")
    assert "waiting: 1 blob(s) restore requested" in capsys.readouterr().err


def test_main_pushes_the_archived_image_and_keep_tags_it(monkeypatch: pytest.MonkeyPatch) -> None:
    world = _World()
    assert world.main(monkeypatch) == 0
    assert world.tags[f"{REPO}:restore-abababababab"] == DIGEST
    assert world.tags[f"{REPO}:biffix-keep-abababababab"] == DIGEST


def test_main_reuses_its_restore_tag_on_a_rerun_after_the_push(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    world = _World()
    world.tags[f"{REPO}:restore-abababababab"] = DIGEST
    assert world.main(monkeypatch) == 0
    assert not world.crane("push")
    assert world.tags[f"{REPO}:biffix-keep-abababababab"] == DIGEST


def test_main_refuses_a_keep_tag_that_names_another_image(monkeypatch: pytest.MonkeyPatch) -> None:
    world = _World()
    world.tags[f"{REPO}:biffix-keep-abababababab"] = "sha256:" + "cd" * 32
    with pytest.raises(SystemExit, match="already names"):
        world.main(monkeypatch)
    assert not world.crane("tag")


def test_main_waits_on_a_restore_already_in_progress(monkeypatch: pytest.MonkeyPatch) -> None:
    world = _World(cold=True)
    world.heads[blob_key("sha256:layer")]["Restore"] = 'ongoing-request="true"'
    assert world.main(monkeypatch) == 2
    assert not [c for c in world.calls if c[:2] == ("s3api", "restore-object")]
    assert not world.crane("push")


def test_main_keep_tags_each_platform_image_of_an_index(monkeypatch: pytest.MonkeyPatch) -> None:
    world = _World()
    platforms = {"sha256:amd": "amd64", "sha256:arm": "arm64"}
    index = {
        "schemaVersion": 2,
        "manifests": [
            {"digest": d, "platform": {"os": "linux", "architecture": a}}
            for d, a in platforms.items()
        ],
    }
    world.objects[blob_key(DIGEST)] = json.dumps(index)
    for digest in platforms:
        world.objects[blob_key(digest)] = json.dumps(
            {"schemaVersion": 2, "config": {"digest": "sha256:cfg"}, "layers": []}
        )
    world.heads = {key: {"ContentLength": len(body)} for key, body in world.objects.items()}
    assert world.main(monkeypatch) == 0
    assert world.tags[f"{REPO}:biffix-keep-abababababab-1"] == "sha256:amd"
    assert world.tags[f"{REPO}:biffix-keep-abababababab-2"] == "sha256:arm"


def test_main_reads_the_archive_bucket_it_is_given(monkeypatch: pytest.MonkeyPatch) -> None:
    # The archive a teardown kept, after a rebuild: the live data stack's bucket is a new, empty
    # one, so the operator names the kept bucket and the stack is not asked.
    world = _World()
    world.bucket = "kept-archive"
    assert world.main(monkeypatch, "--bucket", "kept-archive") == 0
    assert not [c for c in world.calls if c[0] == "cloudformation"]
    assert world.tags[f"{REPO}:restore-abababababab"] == DIGEST


def test_main_names_bucket_when_the_data_stack_is_gone(monkeypatch: pytest.MonkeyPatch) -> None:
    world = _World()

    def aws(*args: str) -> str:
        if args[0] == "cloudformation":
            raise image_restore.AwsError("aws cloudformation list-stack-resources: does not exist")
        return _World.aws(world, *args)

    monkeypatch.setattr(world, "aws", aws)
    with pytest.raises(SystemExit, match="(?s)does not exist.*--bucket"):
        world.main(monkeypatch)


def test_main_asks_for_json_whatever_the_cli_s_default_output(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    world = _World()
    assert world.main(monkeypatch) == 0
    heads = [c for c in world.calls if c[:2] == ("s3api", "head-object")]
    assert heads and all("--output" in c and c[c.index("--output") + 1] == "json" for c in heads)


def test_an_aws_failure_shows_aws_s_own_error(monkeypatch: pytest.MonkeyPatch) -> None:
    def run(cmd: list[str], **_kw: object) -> subprocess.CompletedProcess[str]:
        raise subprocess.CalledProcessError(254, cmd, "", "An error occurred (AccessDenied)")

    monkeypatch.setattr(image_restore.subprocess, "run", run)
    with pytest.raises(
        SystemExit, match=r"aws s3api head-object: An error occurred \(AccessDenied"
    ):
        image_restore._aws("s3api", "head-object")


def test_main_refuses_a_push_that_changed_the_digest(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    world = _World(pushes_as="sha256:" + "cd" * 32)
    assert world.main(monkeypatch) == 1
    assert "differs from " + DIGEST in capsys.readouterr().err
    assert not world.crane("tag")
