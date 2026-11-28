"""Architecture checks at the real conversion boundary, with only Apptainer replaced."""

from __future__ import annotations

import json
import os
import platform
import signal
import subprocess
import sys
import time
from pathlib import Path

import pytest
from executables import python_executable
from helpers import module
from processes import stop_group

LIB = Path(__file__).resolve().parents[1] / "lib"
IMAGE = "quay.io/example/tool@sha256:" + "a" * 64
HOST_ARCH = {"x86_64": "amd64", "arm64": "arm64", "aarch64": "arm64"}[platform.machine()]
OTHER_ARCH = "amd64" if HOST_ARCH == "arm64" else "arm64"

APPTAINER = r"""
import json
import os
import platform
import signal
import subprocess
import sys
import time
from pathlib import Path

if sys.argv[1] == "pull":
    args = [arg for arg in sys.argv[2:] if arg != "--disable-cache"]
    Path(args[0]).write_text(args[0])
    if os.environ.get("CACHE_PRIMED") and "--disable-cache" not in sys.argv:
        print("INFO: Using cached SIF image")
    elif os.environ.get("FALLBACK"):
        suppressed = any(os.environ.get(prefix + flag) == "1"
                         for prefix in ("APPTAINER_", "SINGULARITY_")
                         for flag in ("QUIET", "SILENT"))
        if not suppressed:
            sys.stdout.write("x" * int(os.environ.get("FALLBACK_OFFSET", "0")))
            print("INFO: Architecture not recognized, use native")
            sys.stdout.write("x" * 100000)
    if os.environ.get("DELETE_LOG"):
        Path(os.environ["DELETE_LOG"]).unlink()
elif sys.argv[1:3] == ["sif", "header"]:
    path = Path(sys.argv[3])
    if not path.is_file() or path.read_text() != str(path):
        raise SystemExit("inspection did not name the pulled SIF")
    if os.environ.get("INSPECTED_PATH"):
        Path(os.environ["INSPECTED_PATH"]).write_text(str(path))
    if os.environ.get("INSPECTED_ENV"):
        Path(os.environ["INSPECTED_ENV"]).write_text(json.dumps({
            key: value for key, value in os.environ.items()
            if key.startswith(("APPTAINER_", "SINGULARITY_"))
        }))
    if os.environ.get("WAIT_FOR_PEER") and "bad" in sys.argv[3]:
        deadline = time.monotonic() + 5
        while not Path(os.environ["BLOCK_INSPECTION"]).exists():
            if time.monotonic() >= deadline:
                raise SystemExit("peer inspection never started")
            time.sleep(0.01)
    if os.environ.get("BLOCK_INSPECTION") and (
        not os.environ.get("WAIT_FOR_PEER") or "slow" in sys.argv[3]
    ):
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        child = subprocess.Popen([sys.executable, "-c",
            "import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(60)"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        marker = Path(os.environ["BLOCK_INSPECTION"])
        marker.with_suffix(".tmp").write_text(str(os.getpid()) + " " + str(child.pid))
        marker.with_suffix(".tmp").replace(marker)
        time.sleep(60)
    arch = {"x86_64": "amd64", "arm64": "arm64", "aarch64": "arm64"}[platform.machine()]
    print(os.environ.get("HEADER", "Primary Architecture: " + arch))
    print(os.environ.get("INSPECT_STDERR", ""), file=sys.stderr)
    sys.exit(int(os.environ.get("INSPECT_EXIT", "0")))
else:
    raise SystemExit("unexpected Apptainer arguments: " + repr(sys.argv))
"""


def fake_apptainer(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """Install a specific pull/header stand-in; conversion itself is never mocked."""
    executable = tmp_path / "apptainer"
    python_executable(executable, APPTAINER)
    monkeypatch.setenv("PATH", f"{tmp_path}:{os.environ['PATH']}")
    monkeypatch.syspath_prepend(str(LIB))
    return executable


def conversion(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, **environ: str) -> int:
    fake_apptainer(tmp_path, monkeypatch)
    for key, value in environ.items():
        monkeypatch.setenv(key, value)
    return int(
        module("single_job").convert(
            "",
            str(tmp_path / "images"),
            str(tmp_path / "logs"),
            str(tmp_path / "failure"),
            [IMAGE],
        )
    )


@pytest.mark.parametrize(
    "header",
    [
        f"Primary Architecture: {OTHER_ARCH}",
        "Primary Architecture: unknown",
        "Primary Architecture: 386",
        "no architecture field",
        f"Primary Architecture: {HOST_ARCH}\nPrimary Architecture: {HOST_ARCH}",
    ],
)
def test_conversion_rejects_unusable_architecture_headers(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, header: str
) -> None:
    assert conversion(tmp_path, monkeypatch, HEADER=header) == 1
    assert IMAGE in (tmp_path / "failure").read_text()
    assert list((tmp_path / "images").iterdir()) == []


def test_conversion_publishes_only_a_matching_inspected_image(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    inspected = tmp_path / "inspected.json"
    target = tmp_path / "inspected-path"
    assert (
        conversion(tmp_path, monkeypatch, INSPECTED_ENV=str(inspected), INSPECTED_PATH=str(target))
        == 0
    )
    assert inspected.exists(), "the converted image was published without inspection"
    published = list((tmp_path / "images").iterdir())
    assert len(published) == 1
    assert target.read_text() == str(published[0].with_name(f".{published[0].name}.pulling"))
    assert not (tmp_path / "failure").exists()


def test_conversion_rejects_an_inspection_command_failure(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    assert (
        conversion(
            tmp_path, monkeypatch, INSPECT_EXIT="23", INSPECT_STDERR="cannot read SIF descriptor"
        )
        == 1
    )
    failure = (tmp_path / "failure").read_text()
    assert IMAGE in failure
    assert "23" in failure
    assert "cannot read SIF descriptor" in failure
    assert list((tmp_path / "images").iterdir()) == []


def test_a_cached_sif_cannot_bypass_architecture_recognition(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # A prior pull under another reference can cache the same OCI digest as a host-labelled SIF.
    assert conversion(tmp_path, monkeypatch, CACHE_PRIMED="1", FALLBACK="1") == 1
    assert "recogniz" in (tmp_path / "failure").read_text()
    assert list((tmp_path / "images").iterdir()) == []


def test_a_programming_error_in_inspection_is_not_an_image_failure(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    executable = fake_apptainer(tmp_path, monkeypatch)
    sj = module("single_job")
    images, logs = tmp_path / "images", tmp_path / "logs"
    images.mkdir()
    logs.mkdir()
    converter = sj.Conversion(str(executable), images, logs, sj.pull_env(os.environ))

    def broken_inspection(_image: str, _path: Path) -> str:
        raise RuntimeError("inspection programming error")

    monkeypatch.setattr(converter, "inspect_architecture", broken_inspection)
    with pytest.raises(RuntimeError, match="inspection programming error"):
        converter.pull(IMAGE, {})
    assert not converter.failed.is_set()
    assert list(images.iterdir()) == []


@pytest.mark.parametrize("offset", [0, 65525])
def test_conversion_rejects_a_fallback_notice_outside_the_log_tail(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, offset: int
) -> None:
    assert conversion(tmp_path, monkeypatch, FALLBACK="1", FALLBACK_OFFSET=str(offset)) == 1
    assert "recogniz" in (tmp_path / "failure").read_text()
    assert list((tmp_path / "images").iterdir()) == []


def test_conversion_does_not_treat_an_unreadable_log_as_recognized_architecture(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    name = module("single_job").image_file_name(IMAGE)
    assert conversion(tmp_path, monkeypatch, DELETE_LOG=str(tmp_path / "logs" / f"{name}.log")) == 1
    assert IMAGE in (tmp_path / "failure").read_text()
    assert list((tmp_path / "images").iterdir()) == []


def test_conversion_cannot_inherit_suppressed_architecture_notices(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    flags = {
        prefix + flag: "1"
        for prefix in ("APPTAINER_", "SINGULARITY_")
        for flag in ("QUIET", "SILENT", "DEBUG", "VERBOSE")
    }
    assert conversion(tmp_path, monkeypatch, FALLBACK="1", **flags) == 1
    assert "recogniz" in (tmp_path / "failure").read_text()


def test_inspection_excludes_credentials_but_preserves_requested_diagnostics(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    inspected = tmp_path / "inspected.json"
    assert (
        conversion(
            tmp_path,
            monkeypatch,
            INSPECTED_ENV=str(inspected),
            APPTAINER_DOCKER_PASSWORD="private-password",
            SINGULARITY_DOCKER_USERNAME="private-user",
            APPTAINER_DEBUG="1",
            SINGULARITY_VERBOSE="1",
            APPTAINER_MESSAGELEVEL="5",
        )
        == 0
    )
    environ = json.loads(inspected.read_text())
    assert environ["APPTAINER_MESSAGELEVEL"] == "1"
    assert not any(key.endswith(("PASSWORD", "USERNAME")) for key in environ)
    assert environ["APPTAINER_DEBUG"] == "1"
    assert environ["SINGULARITY_VERBOSE"] == "1"


@pytest.mark.parametrize(
    ("machine", "expected"),
    [
        ("x86_64", "amd64"),
        ("amd64", "amd64"),
        ("aarch64", "arm64"),
        ("arm64", "arm64"),
    ],
)
def test_host_architecture_uses_oci_names(machine: str, expected: str) -> None:
    assert module("single_job").host_architecture(machine) == expected


@pytest.mark.parametrize(
    "header",
    [
        "Primary Architecture: arm64\n",
        "Version: 01\nPrimary Architecture:\t  arm64\n",
        "Primary Architecture:              arm64\n",
    ],
)
def test_sif_architecture_accepts_pinned_header_whitespace(header: str) -> None:
    assert module("single_job").sif_architecture(header) == "arm64"


@pytest.mark.parametrize(
    "header",
    [
        "Primary Architecture: arm64\nPrimary Architecture:\n",
        "Primary Architecture:arm64\n",
    ],
)
def test_sif_architecture_rejects_malformed_or_empty_duplicate_fields(header: str) -> None:
    with pytest.raises(ValueError):
        module("single_job").sif_architecture(header)


def test_a_mismatch_stops_a_peer_inspection_that_ignores_sigterm(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake_apptainer(tmp_path, monkeypatch)
    marker = tmp_path / "peer.pid"
    proc = subprocess.Popen(
        [
            sys.executable,
            str(LIB / "single_job.py"),
            "convert",
            "",
            str(tmp_path / "images"),
            str(tmp_path / "logs"),
            str(tmp_path / "failure"),
            "quay.io/example/bad:1",
            "quay.io/example/slow:1",
        ],
        env=os.environ
        | {
            "BLOCK_INSPECTION": str(marker),
            "WAIT_FOR_PEER": "1",
            "HEADER": f"Primary Architecture: {OTHER_ARCH}",
            "BIFFIX_SIF_PARALLEL": "2",
        },
        start_new_session=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        proc.communicate(timeout=8)
        assert proc.returncode == 1
        assert marker.exists(), "the test did not exercise a concurrent inspection"
        assert "does not match host" in (tmp_path / "failure").read_text()
        assert list((tmp_path / "images").iterdir()) == []
        parent, child = map(int, marker.read_text().split())
        listing = subprocess.run(
            ["ps", "-A", "-o", "pid=,stat="], capture_output=True, text=True, check=True
        ).stdout
        states = {
            int(parts[0]): parts[1]
            for line in listing.splitlines()
            if len(parts := line.split()) == 2
        }
        assert all(pid not in states or states[pid].startswith("Z") for pid in (parent, child))
    finally:
        stop_group(proc)
        if marker.exists():
            parent = int(marker.read_text().split()[0])
            try:
                os.killpg(parent, signal.SIGKILL)
            except ProcessLookupError:
                pass


def test_cancel_during_inspection_kills_its_process_group(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake_apptainer(tmp_path, monkeypatch)
    marker = tmp_path / "inspection.pid"
    proc = subprocess.Popen(
        [
            sys.executable,
            str(LIB / "single_job.py"),
            "convert",
            "",
            str(tmp_path / "images"),
            str(tmp_path / "logs"),
            str(tmp_path / "failure"),
            IMAGE,
        ],
        env=os.environ | {"BLOCK_INSPECTION": str(marker)},
        start_new_session=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        deadline = time.monotonic() + 5
        while not marker.exists():
            assert proc.poll() is None, stop_group(proc)
            assert time.monotonic() < deadline, "inspection never started"
            time.sleep(0.01)
        parent, child = map(int, marker.read_text().split())
        proc.send_signal(signal.SIGTERM)
        proc.communicate(timeout=5)
        assert proc.returncode == 143
        assert not (tmp_path / "failure").exists()
        assert list((tmp_path / "images").iterdir()) == []
        # A dead process may briefly be an unreaped zombie; no running group member may survive.
        listing = subprocess.run(
            ["ps", "-A", "-o", "pid=,stat="], capture_output=True, text=True, check=True
        ).stdout
        states = {
            int(parts[0]): parts[1]
            for line in listing.splitlines()
            if len(parts := line.split()) == 2
        }
        assert all(pid not in states or states[pid].startswith("Z") for pid in (parent, child))
    finally:
        stop_group(proc)
        if marker.exists():
            parent = int(marker.read_text().split()[0])
            try:
                os.killpg(parent, signal.SIGKILL)
            except ProcessLookupError:
                pass
