"""The projects section: each project's bucket and key, and the data sources its runs read
(ProjectStack, the api stack's per-project coordinator and context)."""

from __future__ import annotations

import re
from dataclasses import dataclass

from biffix_infra.config.fields import Fields, Raw, flag, optional_text, text

DATA_SOURCE_KINDS = ("data", "references")

# A project id names the `project-<id>` CloudFormation stack, whose name allows only letters,
# digits and `-` (its SSM context parameter, `/biffix/<tenant>/projects/<id>`, allows more).
_PROJECT_ID = re.compile(r"[A-Za-z0-9-]+")


@dataclass(frozen=True)
class DataSource:
    """An S3 location a project's runs may read inputs from, offered by name in the launch form's
    file picker (a sequencing core's delivery folder, a reference library).

    Registered here — never at run time — because each one is an IAM grant: the project's run
    role, the coordinator and the backend get read (and list, under `prefix`) on exactly this
    location. biffix never writes to it. A cross-account bucket additionally needs a bucket policy
    on its own side allowing those roles.
    """

    id: str
    name: str
    bucket: str
    # Key prefix under the bucket; "" is the whole bucket. Normalized to end in "/".
    prefix: str = ""
    description: str | None = None
    # "data" (inputs) or "references" (reference files).
    kind: str = "data"
    # The CMK the objects are encrypted with, when they are (SSE-S3 needs none).
    key_arn: str | None = None
    # The bucket's owner has applied the alignment viewer's GET/HEAD CORS rule
    # (`viewer_cors_rule`), so the browser may read reference files from it directly. biffix never
    # applies it to a bucket it does not own; the deploy prints the command. Only a hint for the
    # viewer's message: the browser's own request is the test.
    viewer_cors_applied: bool = False

    def to_backend(self) -> dict[str, str | bool]:
        """The entry the backend reads from its project's context (`data_sources`, in SSM)."""
        entry: dict[str, str | bool] = {
            "id": self.id,
            "name": self.name,
            "bucket": self.bucket,
            "prefix": self.prefix,
            "kind": self.kind,
        }
        if self.description:
            entry["description"] = self.description
        if self.viewer_cors_applied:
            entry["viewer_cors"] = True
        return entry


@dataclass(frozen=True)
class Project:
    id: str
    # Optional deployer-provided KMS key ARN for this project's data (BYOK). None => biffix
    # creates a per-project customer-managed key (manage-AWS-through-AWS: the deployer may prefer
    # to own/rotate/audit the key in their own key infrastructure).
    key_arn: str | None = None
    # Optional existing S3 bucket to reference in place (migration). None => biffix creates a
    # managed bucket. When set, biffix imports and grants access but never owns/mutates it.
    bucket_name: str | None = None
    # Other S3 locations this project's runs read inputs from (see DataSource).
    data_sources: tuple[DataSource, ...] = ()
    # For an IMPORTED bucket only: its owner has applied the browser-upload CORS rule (and the
    # abort-incomplete-uploads lifecycle rule) that `upload_bucket_settings` describes, so the UI
    # may offer uploads. biffix never applies them to a bucket it does not own; a created bucket
    # gets them from ProjectStack and ignores this.
    uploads_cors_applied: bool = False
    # For an IMPORTED bucket only: its owner has applied the alignment viewer's GET/HEAD CORS rule
    # (`viewer_cors_rule`), so the viewer may read alignments from it directly. A created bucket
    # gets it from ProjectStack and ignores this.
    viewer_cors_applied: bool = False


def parse_data_source(raw: Raw) -> DataSource:
    """One `data_sources` entry, its prefix normalized (null or "" is the whole bucket)."""
    for key in ("id", "name", "bucket"):
        if not raw.get(key):
            raise ValueError(f"data source {raw!r} requires `{key}`")
    where = f"data source {raw['id']!r}:"
    source_id = text(f"{where} id", raw["id"])
    if source_id == "project":
        raise ValueError("data source id `project` is reserved for the project bucket")
    kind = raw.get("kind") or "data"
    if kind not in DATA_SOURCE_KINDS:
        raise ValueError(f"data source `{source_id}`: kind must be data or references")
    prefix = raw.get("prefix")
    if prefix is not None and not isinstance(prefix, str):
        raise ValueError(f"{where} prefix {prefix!r}: expected a key prefix (a string)")
    prefix = (prefix or "").strip("/")
    return DataSource(
        id=source_id,
        name=text(f"{where} name", raw["name"]),
        bucket=text(f"{where} bucket", raw["bucket"]),
        prefix=f"{prefix}/" if prefix else "",
        description=optional_text(f"{where} description", raw.get("description")),
        kind=kind,
        key_arn=optional_text(f"{where} key_arn", raw.get("key_arn")),
        viewer_cors_applied=flag(f"{where} viewer_cors_applied", raw.get("viewer_cors_applied")),
    )


def parse_project(raw: Raw) -> Project:
    """One `projects` entry, with its data sources (unique ids). Its id is checked with the
    others' (`validate_projects`)."""
    where = f"project {raw['id']!r}:"
    sources = tuple(parse_data_source(d) for d in raw.get("data_sources") or [])
    ids = [s.id for s in sources]
    if len(ids) != len(set(ids)):
        raise ValueError(f"project `{raw['id']}` has duplicate data source ids: {ids}")
    return Project(
        id=raw["id"],
        key_arn=optional_text(f"{where} key_arn", raw.get("key_arn")),
        bucket_name=optional_text(f"{where} bucket_name", raw.get("bucket_name")),
        data_sources=sources,
        uploads_cors_applied=flag(f"{where} uploads_cors_applied", raw.get("uploads_cors_applied")),
        viewer_cors_applied=flag(f"{where} viewer_cors_applied", raw.get("viewer_cors_applied")),
    )


def validate_projects(projects: list[Project]) -> None:
    """Refuse a project list the backend could not route by: an id outside `[A-Za-z0-9-]` (it
    names the `project-<id>` stack) or starting with `rt-`, two projects with one id, or two
    projects on one bucket. The
    backend resolves a run's project from the bucket its outputs are in, and each project's
    coordinator and run roles are scoped to its own bucket, so a shared bucket would make two
    projects one isolation boundary."""
    ids = [p.id for p in projects]
    for pid in ids:
        if not isinstance(pid, str) or not _PROJECT_ID.fullmatch(pid):
            raise ValueError(f"project id {pid!r}: use only letters, digits and `-`")
        # Its coordinator template, `biffix-<tenant>-coordinator-<id>`, must stay outside the
        # name space of the variants the backend registers (`…-coordinator-rt-*`).
        if pid.startswith("rt-"):
            raise ValueError(f"project id {pid!r}: may not start with `rt-` (reserved)")
    duplicates = sorted({pid for pid in ids if ids.count(pid) > 1})
    if duplicates:
        raise ValueError(f"duplicate project ids: {duplicates}")
    owners: dict[str, list[str]] = {}
    for project in projects:
        if project.bucket_name:
            owners.setdefault(project.bucket_name, []).append(project.id)
    shared = {bucket: pids for bucket, pids in owners.items() if len(pids) > 1}
    if shared:
        raise ValueError(
            "projects may not share a bucket (each project is its own isolation boundary): "
            + "; ".join(f"{bucket} is used by {', '.join(pids)}" for bucket, pids in shared.items())
        )


def parse(raw: Raw) -> Fields:
    """`projects` (required, at least one), as the DeployConfig field."""
    if not raw["projects"]:
        raise ValueError("projects: at least one project is required")
    projects = [parse_project(p) for p in raw["projects"]]
    validate_projects(projects)
    return {"projects": projects}
