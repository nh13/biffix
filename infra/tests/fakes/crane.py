"""A stand-in for `crane` over a JSON file registry, for the CodeBuild script tests.

Images are JSON objects (an image: layers + config; an index: manifests) named by the sha256 of
their JSON. Tags with the workflow repository's immutable prefixes (IMMUTABLE: its
`WORKFLOW_IMMUTABLE_TAGS`, which test_codebuild_scripts checks it equals) can be written once only,
as ECR's MUTABLE_WITH_EXCLUSION setting enforces for them; in the builds repository
($FAKE_BUILDS_REPO) every tag but the mutable ones (BUILDS_MUTABLE: its `BUILDS_MUTABLE_TAGS`) can,
as its IMMUTABLE_WITH_EXCLUSION setting enforces. Every tag write is logged, so a test can
check none is written twice. `crane blob` prints the blob's digest, and `crane append` takes the
appended layer's digest from the file it is given (so a layer "downloaded" by blob appends as
itself). `crane export` writes a tar of the files the state's `layer_files` lists for each of the
image's layers. `crane copy` tags the source's digest into the target, or, with
$FAKE_CRANE_COPY_CHANGES_DIGEST=1, a changed copy of it (as a registry that rewrites a manifest).

State: $FAKE_REGISTRY (a JSON file, created on first use). The tests load this module to seed it
(`load`, `put`, `save`), so it has no import-time side effects."""

from __future__ import annotations

import csv
import hashlib
import io
import json
import os
import sys
import tarfile
from typing import Any

IMMUTABLE = ("biffix-keep-", "import-", "derived-", "restore-")
BUILDS_MUTABLE = ("scratch-",)

# The registry's state ($FAKE_REGISTRY): `images`, `tags`, `writes` and `layer_files`, as JSON.
State = dict[str, Any]
# One image (`layers`, `config`, `arch`) or index (`kind: index`, `manifests`), as JSON.
Obj = dict[str, Any]


def load(path: str | None = None) -> State:
    path = path or os.environ["FAKE_REGISTRY"]
    if not os.path.exists(path):
        return {"images": {}, "tags": {}, "writes": [], "layer_files": {}}
    with open(path) as fh:
        state: State = json.load(fh)
    return state


def save(state: State, path: str | None = None) -> None:
    with open(path or os.environ["FAKE_REGISTRY"], "w") as fh:
        json.dump(state, fh, indent=1, sort_keys=True)


def put(state: State, obj: Obj) -> str:
    digest = "sha256:" + hashlib.sha256(json.dumps(obj, sort_keys=True).encode()).hexdigest()
    state["images"][digest] = obj
    return digest


def split(ref: str) -> tuple[str, str | None, str | None]:
    """(repo, tag, digest) of `repo:tag` or `repo@digest`."""
    if "@" in ref:
        repo, digest = ref.split("@", 1)
        return repo, None, digest
    repo, _, tag = ref.rpartition(":")
    if "/" in tag or not repo:
        sys.exit(f"fake crane: not a tagged reference: {ref}")
    return repo, tag, None


def target(ref: str | None) -> tuple[str, str]:
    """(repo, tag) of the `repo:tag` an image is written to."""
    repo, name, _ = split(ref or "")
    if name is None:
        sys.exit(f"fake crane: not a tagged target: {ref}")
    return repo, name


def resolve(state: State, ref: str) -> str | None:
    repo, tag, digest = split(ref)
    if digest is not None:
        return digest if digest in state["images"] else None
    tagged: str | None = state["tags"].get(f"{repo}:{tag}")
    return tagged


def tag(state: State, repo: str, name: str, digest: str) -> None:
    key = f"{repo}:{name}"
    if repo == os.environ.get("FAKE_BUILDS_REPO"):
        immutable = not name.startswith(BUILDS_MUTABLE)
    else:
        immutable = name.startswith(IMMUTABLE)
    if immutable and key in state["tags"]:
        sys.exit(f"fake crane: tag {name} is immutable and exists (names {state['tags'][key]})")
    state["tags"][key] = digest
    state["writes"].append(key)


def image(state: State, ref: str) -> tuple[str, Obj]:
    digest = resolve(state, ref)
    if digest is None:
        sys.exit(f"fake crane: MANIFEST_UNKNOWN {ref}")
    return digest, state["images"][digest]


def pick_platform(state: State, obj: Obj, platform: str | None) -> Obj:
    if obj["kind"] != "index" or platform is None:
        return obj
    arch = platform.split("/")[1]
    for m in obj["manifests"]:
        if m["platform"]["architecture"] == arch:
            picked: Obj = state["images"][m["digest"]]
            return picked
    sys.exit(f"fake crane: no {platform} in the index")


def csv_record(text: str) -> list[str]:
    return next(csv.reader(io.StringIO(text)))


def main(argv: list[str]) -> None:
    state = load()
    platform = None
    if "--platform" in argv:
        i = argv.index("--platform")
        platform = argv[i + 1]
        del argv[i : i + 2]
    cmd, args = argv[0], argv[1:]
    if cmd == "digest":
        digest = resolve(state, args[0])
        if digest is None:
            sys.exit(1)
        if platform:
            obj = pick_platform(state, state["images"][digest], platform)
            digest = next(d for d, o in state["images"].items() if o == obj)
        print(digest)
    elif cmd == "manifest":
        _, obj = image(state, args[0])
        if obj["kind"] == "index":
            print(json.dumps({"manifests": obj["manifests"]}))
        else:
            print(json.dumps({"layers": [{"digest": d} for d in obj["layers"]]}))
    elif cmd == "config":
        _, obj = image(state, args[0])
        obj = pick_platform(state, obj, platform)
        print(json.dumps({"architecture": obj["arch"], "config": obj["config"]}))
    elif cmd == "blob":
        print(split(args[0])[2])
    elif cmd == "export":
        _, obj = image(state, args[0])
        obj = pick_platform(state, obj, platform)
        assert args[1] == "-", args
        with tarfile.open(fileobj=sys.stdout.buffer, mode="w|") as tar:
            for layer in obj["layers"]:
                for name in state.get("layer_files", {}).get(layer, []):
                    info = tarfile.TarInfo(name)
                    info.type = tarfile.DIRTYPE if name.endswith("/") else tarfile.REGTYPE
                    tar.addfile(info, io.BytesIO(b""))
        return
    elif cmd == "copy":
        digest, obj = image(state, args[0])
        if os.environ.get("FAKE_CRANE_COPY_CHANGES_DIGEST") == "1":
            digest = put(state, {**obj, "rewritten": True})
        tag(state, *target(args[1]), digest)
    elif cmd == "tag":
        digest, _ = image(state, args[0])
        tag(state, split(args[0])[0], args[1], digest)
    elif cmd == "append":
        opts = dict(zip(args[::2], args[1::2], strict=True))
        _, base = image(state, opts["-b"])
        with open(opts["-f"]) as fh:
            layer = fh.read().strip()
        new = {**base, "layers": base["layers"] + [layer]}
        tag(state, *target(opts["-t"]), put(state, new))
    elif cmd == "mutate":
        ref, rest = args[0], args[1:]
        opts = dict(zip(rest[::2], rest[1::2], strict=True))
        _, obj = image(state, ref)
        config = dict(obj["config"])
        if "--entrypoint" in opts:
            config["Entrypoint"] = csv_record(opts["--entrypoint"])
            config["Cmd"] = []
        if "--cmd" in opts:
            config["Cmd"] = csv_record(opts["--cmd"])
        tag(state, *target(opts["-t"]), put(state, {**obj, "config": config}))
    elif cmd == "index" and args[0] == "append":
        rest = args[1:]
        manifests: list[Obj] = []
        dest = None
        for flag, value in zip(rest[::2], rest[1::2], strict=True):
            if flag == "-m":
                digest, obj = image(state, value)
                platform_of = {"os": "linux", "architecture": obj["arch"]}
                manifests.append({"digest": digest, "platform": platform_of})
            elif flag == "-t":
                dest = value
        tag(state, *target(dest), put(state, {"kind": "index", "manifests": manifests}))
    else:
        sys.exit(f"fake crane: unsupported {argv}")
    save(state)


if __name__ == "__main__":
    main(sys.argv[1:])
