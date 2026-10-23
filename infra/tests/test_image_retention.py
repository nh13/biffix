"""The image-retention plan: protect what is deployed, prune old build tags, never touch pins."""

import json
import subprocess
from datetime import UTC, datetime

import pytest

from biffix_infra import image_retention
from biffix_infra.image_retention import (
    COORDINATOR_BUILD,
    Image,
    plan_retention,
    protect_tag,
    pushed_at,
)

SHA = [f"{i:x}" * 40 for i in range(1, 9)]  # 40-hex build names, oldest first


def day(n: int) -> datetime:
    return datetime(2026, 9, n, tzinfo=UTC)


def build(n: int, prefix: str = "") -> list[Image]:
    """A multi-arch build: an index per arch and a top index (tagged), pushed on day n."""
    name = f"{prefix}{SHA[n]}"
    return [
        Image(f"sha256:{n}0", (name,), day(10 + n)),
        Image(f"sha256:{n}1", (f"{name}-amd64",), day(10 + n)),
        Image(f"sha256:{n}2", (f"{name}-arm64",), day(10 + n)),
    ]


def test_old_builds_lose_their_tags_but_the_deployed_and_newest_keep_them() -> None:
    images = [img for n in range(8) for img in build(n)]
    plan = plan_retention(
        images,
        deployed_tag=SHA[1],  # an old build is deployed
        children={"sha256:10": ["sha256:c1", "sha256:c2"]},
        protect_prefix="deployed-x-",
        keep=2,
    )
    deleted_builds = {t.split("-")[0] for t in plan.delete}
    # Kept: the deployed build (1) and the two newest (6, 7).
    assert deleted_builds == {SHA[n] for n in (0, 2, 3, 4, 5)}
    assert len(plan.delete) == 5 * 3
    # The deployed build's images and the images its index lists are protected.
    protected = {d for d, _ in plan.add}
    assert protected == {"sha256:10", "sha256:11", "sha256:12", "sha256:c1", "sha256:c2"}
    assert ("sha256:c1", protect_tag("deployed-x-", "sha256:c1")) in plan.add


def test_pins_and_other_tags_are_never_removed_and_stale_protection_is_dropped() -> None:
    pinned = Image("sha256:p", (SHA[0], "biffix-pin-abc"), day(1))
    # A coordinator variant (base + engine runtime) the backend built: not a build tag.
    variant = Image("sha256:v", ("coordinator-0123456789ab-cdef01234567",), day(1))
    stale = Image("sha256:s", ("deployed-x-oldoldold",), day(2))
    images = [pinned, variant, stale, *[img for n in range(1, 4) for img in build(n)]]
    plan = plan_retention(
        images,
        deployed_tag=SHA[3],
        children={},
        build_pattern=COORDINATOR_BUILD,
        protect_prefix="deployed-x-",
        keep=1,
    )
    # Exactly: the pinned image's old build tag (its pin tag keeps the image), build 1 (older than
    # the one newest build kept besides the deployed one), and the previous deploy's protection.
    assert set(plan.delete) == {
        SHA[0],
        SHA[1],
        f"{SHA[1]}-amd64",
        f"{SHA[1]}-arm64",
        "deployed-x-oldoldold",
    }
    assert len(plan.delete) == len(set(plan.delete))
    # The current deploy's images are protected.
    assert {d for d, _ in plan.add} == {"sha256:30", "sha256:31", "sha256:32"}


def test_an_existing_protection_tag_is_not_added_again() -> None:
    images = [
        *build(1),
        Image("sha256:10", (SHA[1], protect_tag("deployed-x-", "sha256:10")), day(11)),
    ]
    images = [i for i in images if not (i.digest == "sha256:10" and len(i.tags) == 1)]
    plan = plan_retention(
        images,
        deployed_tag=SHA[1],
        children={},
        protect_prefix="deployed-x-",
        keep=5,
    )
    assert [d for d, _ in plan.add] == ["sha256:11", "sha256:12"]
    assert plan.delete == []


def test_the_last_tag_of_an_image_a_kept_index_lists_is_never_removed() -> None:
    """Removing an image's last tag deletes it at once, so a platform image that a kept index lists
    keeps one tag even when it is stale protection or an old build tag."""
    # The previous deploy (build 1) protected its untagged platform images; build 2 is deployed now.
    # Build 1's index is still kept (keep=1), and a `v17` release index lists build 0's platforms.
    old_children = [
        Image("sha256:1a", (protect_tag("deployed-x-", "sha256:1a"),), day(11)),
        Image("sha256:1b", (protect_tag("deployed-x-", "sha256:1b"),), day(11)),
    ]
    images = [
        Image("sha256:10", (SHA[1], protect_tag("deployed-x-", "sha256:10")), day(11)),
        *old_children,
        Image("sha256:20", (SHA[2],), day(12)),
        Image("sha256:v17", ("v17",), day(9)),
        Image("sha256:00", (f"{SHA[0]}-amd64",), day(9)),
        Image("sha256:0x", (SHA[0], "biffix-pin-0"), day(9)),
    ]
    plan = plan_retention(
        images,
        deployed_tag=SHA[2],
        children={"sha256:10": ["sha256:1a", "sha256:1b"], "sha256:v17": ["sha256:00"]},
        protect_prefix="deployed-x-",
        keep=1,
    )
    # Build 1 is still kept, so its platform images keep their (stale) protection tags; the
    # index's own stale protection goes, since its build tag keeps it.
    assert set(plan.delete) == {protect_tag("deployed-x-", "sha256:10"), SHA[0]}
    # `v17` lists build 0's amd64 image, whose build tag is its last: it stays.
    assert f"{SHA[0]}-amd64" not in plan.delete


def test_a_kept_image_is_released_once_nothing_kept_lists_it() -> None:
    images = [
        Image("sha256:1a", (protect_tag("deployed-x-", "sha256:1a"),), day(11)),
        Image("sha256:20", (SHA[2],), day(12)),
    ]
    plan = plan_retention(
        images, deployed_tag=SHA[2], children={}, protect_prefix="deployed-x-", keep=1
    )
    assert plan.delete == [protect_tag("deployed-x-", "sha256:1a")]


def test_builds_are_ordered_by_instant_across_a_dst_change() -> None:
    """The AWS CLI renders imagePushedAt in the local timezone: across the end of DST, the newer
    push has the lexically SMALLER string, so the pushes must be compared as instants."""
    older = pushed_at("2026-11-01T01:50:00-07:00")  # 08:50 UTC
    newer = pushed_at("2026-11-01T01:10:00-08:00")  # 09:10 UTC
    assert "2026-11-01T01:10:00-08:00" < "2026-11-01T01:50:00-07:00"
    assert newer > older
    images = [
        Image("sha256:a", (SHA[0],), older),
        Image("sha256:b", (SHA[1],), newer),
        Image("sha256:c", (SHA[2],), day(1)),
    ]
    plan = plan_retention(
        images, deployed_tag=SHA[2], children={}, protect_prefix="deployed-x-", keep=1
    )
    assert plan.delete == [SHA[0]]


def test_pushed_at_accepts_the_cli_v1_epoch_and_a_naive_time() -> None:
    assert pushed_at(0) == datetime(1970, 1, 1, tzinfo=UTC)
    assert pushed_at("2026-09-01T00:00:00") == day(1)


# ── main, against a stand-in `aws ecr` ──

MANIFEST = "application/vnd.oci.image.manifest.v1+json"


def _ecr(
    monkeypatch: pytest.MonkeyPatch,
    images: list[dict[str, object]],
    put_error: str | None = None,
    delete_failure: str | None = None,
) -> list[tuple[str, ...]]:
    """Answer `image_retention._aws` from `images`; record every call. `delete_failure` is the
    failureCode batch-delete-image reports for every tag (it exits 0 all the same)."""
    calls: list[tuple[str, ...]] = []

    def aws(*args: str) -> dict[str, object]:
        calls.append(args)
        if args[0] == "describe-images":
            return {"imageDetails": images}
        if args[0] == "batch-get-image":
            return {"images": [{"imageManifest": "{}", "imageManifestMediaType": MANIFEST}]}
        if args[0] == "put-image" and put_error:
            raise subprocess.CalledProcessError(254, ["aws", *args], stderr=put_error)
        if args[0] == "batch-delete-image" and delete_failure:
            tag = args[-1].removeprefix("imageTag=")
            failure = {"imageTag": tag}, delete_failure, "refused"
            keys = ("imageId", "failureCode", "failureReason")
            return {"imageIds": [], "failures": [dict(zip(keys, failure, strict=True))]}
        return {}

    monkeypatch.setattr(image_retention, "_aws", aws)
    return calls


def _detail(digest: str, tag: str, n: int) -> dict[str, object]:
    return {
        "imageDigest": digest,
        "imageTags": [tag],
        "imagePushedAt": f"2026-09-{n:02d}T00:00:00+00:00",
        "imageManifestMediaType": MANIFEST,
    }


ARGS = ["--repo", "r", "--protect-prefix", "deployed-x-", "--keep", "0"]
TWO_BUILDS = [_detail("sha256:1", SHA[1], 1), _detail("sha256:2", SHA[2], 2)]


def test_main_prunes_nothing_when_the_deployed_tag_is_missing(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    calls = _ecr(monkeypatch, TWO_BUILDS)
    assert image_retention.main([*ARGS, "--deployed-tag", SHA[3]]) == 1
    assert [c[0] for c in calls] == ["describe-images"]


def test_main_tolerates_a_protection_tag_that_already_exists(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    calls = _ecr(monkeypatch, TWO_BUILDS, put_error="An error (ImageAlreadyExistsException)")
    assert image_retention.main([*ARGS, "--deployed-tag", SHA[2]]) == 0
    # The put failed benignly, and the old build's tag was still removed.
    writes = [c for c in calls if c[0] in ("put-image", "batch-delete-image")]
    assert [c[0] for c in writes] == ["put-image", "batch-delete-image"]
    assert writes[-1][-1] == f"imageTag={SHA[1]}"


def test_main_protects_the_platform_images_the_deployed_index_lists(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # main reads every index (and every image whose media type ECR does not report) to learn
    # what it lists: without that, the deployed index's untagged platform images are unprotected.
    index = "application/vnd.oci.image.index.v1+json"
    images = [
        {**_detail("sha256:top", SHA[2], 2), "imageManifestMediaType": index},
        {**_detail("sha256:amd", "unused", 2), "imageTags": []},
        {**_detail("sha256:arm", "unused", 2), "imageTags": []},
        {
            k: v
            for k, v in _detail("sha256:old", SHA[1], 1).items()
            if k != "imageManifestMediaType"
        },
    ]
    manifests = {
        "sha256:top": {"manifests": [{"digest": "sha256:amd"}, {"digest": "sha256:arm"}]},
        "sha256:old": {"schemaVersion": 2},
    }
    calls: list[tuple[str, ...]] = []

    def aws(*args: str) -> dict[str, object]:
        calls.append(args)
        if args[0] == "describe-images":
            return {"imageDetails": images}
        if args[0] == "batch-get-image":
            digest = args[args.index("--image-ids") + 1].removeprefix("imageDigest=")
            media = index if digest == "sha256:top" else MANIFEST
            manifest = json.dumps(manifests.get(digest, {}))
            return {"images": [{"imageManifest": manifest, "imageManifestMediaType": media}]}
        return {}

    monkeypatch.setattr(image_retention, "_aws", aws)
    assert image_retention.main([*ARGS, "--deployed-tag", SHA[2]]) == 0
    # The index and the untyped image are read; the typed platform images are not.
    read = {c[c.index("--image-ids") + 1] for c in calls if c[0] == "batch-get-image"}
    assert {"imageDigest=sha256:top", "imageDigest=sha256:old"} <= read
    puts = {
        (c[c.index("--image-digest") + 1], c[c.index("--image-tag") + 1])
        for c in calls
        if c[0] == "put-image"
    }
    assert {(d, protect_tag("deployed-x-", d)) for d in ("sha256:amd", "sha256:arm")} <= puts


def test_main_fails_on_a_tag_ecr_refused_to_remove(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    # batch-delete-image exits 0 and reports a refused tag only in its `failures`.
    _ecr(monkeypatch, TWO_BUILDS, delete_failure="InvalidImageTag")
    assert image_retention.main([*ARGS, "--deployed-tag", SHA[2]]) == 1
    err = capsys.readouterr().err
    assert SHA[1] in err and "InvalidImageTag" in err, err


def test_main_tolerates_a_tag_already_gone(monkeypatch: pytest.MonkeyPatch) -> None:
    _ecr(monkeypatch, TWO_BUILDS, delete_failure="ImageNotFound")
    assert image_retention.main([*ARGS, "--deployed-tag", SHA[2]]) == 0


def test_main_stops_on_any_other_put_error_and_shows_it(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    calls = _ecr(monkeypatch, TWO_BUILDS, put_error="An error (AccessDeniedException)")
    assert image_retention.main([*ARGS, "--deployed-tag", SHA[2]]) == 1
    assert "batch-delete-image" not in [c[0] for c in calls]
    # AWS's own error, not a bare "returned non-zero exit status".
    err = capsys.readouterr().err
    assert "aws put-image" in err and "AccessDeniedException" in err, err
