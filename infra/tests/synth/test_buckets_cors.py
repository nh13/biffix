"""Data sources, browser uploads and the CORS rules the deploy applies or prints for buckets it
does not own."""

import dataclasses
import fnmatch
import json
from pathlib import Path
from typing import Any, Literal

import aws_cdk as cdk
from aws_cdk.assertions import Match, Template

from biffix_infra.app_graph import Stacks
from biffix_infra.config import (
    DataSource,
    Project,
    bucket_cors_instructions,
    noncurrent_versions_lifecycle_rule,
    upload_bucket_settings,
    viewer_cors_rule,
)
from biffix_infra.stacks.identity_stack import IdentityStack
from biffix_infra.stacks.project_stack import ProjectStack
from tests.synth.synth_helpers import (
    CONFIG,
    CORE,
    Json,
    _actions,
    _bucket_objects,
    _build,
    _grants,
    _project_context,
    _resources,
    _statements,
    _statements_for,
)


def test_run_role_reads_only_under_a_data_sources_prefix() -> None:
    app = cdk.App()
    ident = IdentityStack(app, "ident")
    proj = ProjectStack(
        app,
        "proj",
        project_id="p",
        permissions_boundary=ident.permissions_boundary,
        bucket_name="proj-bucket",
        data_sources=(CORE,),
    )
    statements = _statements(Template.from_stack(proj))
    gets = [st for st in statements if _grants(st, "s3:GetObject")]
    # Only under the prefix: the data source's objects outside it, or its whole bucket, leak.
    assert {r for st in gets for r in _resources(st) if "seq-core" in r} == {
        "arn:aws:s3:::seq-core/deliveries/*"
    }
    lists = [
        st
        for st in statements
        if _actions(st) == {"s3:ListBucket"} and _resources(st) == ["arn:aws:s3:::seq-core"]
    ]
    assert lists[0]["Condition"] == {"StringLike": {"s3:prefix": ["deliveries/*"]}}
    decrypt = [st for st in statements if _actions(st) == {"kms:Decrypt"}]
    assert decrypt[0]["Resource"] == CORE.key_arn
    # Read only: nothing writes to a data source.
    assert not any(
        _grants(st, "s3:PutObject") and "seq-core" in json.dumps(st["Resource"])
        for st in statements
    )


def test_backend_and_coordinator_read_data_sources_and_backend_knows_them(tmp_path: Path) -> None:
    project = dataclasses.replace(CONFIG.projects[0], data_sources=(CORE,))
    cfg = dataclasses.replace(CONFIG, projects=[project, *CONFIG.projects[1:]])
    stacks = _build(tmp_path, config=cfg)
    api = stacks["api"]
    t = Template.from_stack(api)
    for role in (api.coordinator_roles[project.id], api.backend_lambda.role):
        resources = {
            r
            for st in _statements_for(t, api, role)
            if _grants(st, "s3:GetObject")
            for r in _resources(st)
            if "seq-core" in r
        }
        assert resources == {"arn:aws:s3:::seq-core/deliveries/*"}, (role, resources)
    # The backend learns them from the project's context.
    assert _project_context(api, project.id)["data_sources"] == [CORE.to_backend()]


def test_backend_writes_only_under_run_inputs_staging_and_uploads(default_stacks: Stacks) -> None:
    api = default_stacks["api"]
    t = Template.from_stack(api)
    # Each resource of every PutObject statement, one by one: exactly these three prefixes of the
    # default project's bucket (a leaked whole-bucket resource, or a missing one, fails).
    puts = {
        r
        for st in _statements_for(t, api, api.backend_lambda.role)
        if _grants(st, "s3:PutObject")
        for r in _resources(st)
    }
    assert _bucket_objects(puts, default_stacks["projects"][0]) == {
        "biffix/runs/*/inputs/*",
        "workflow-sources/*",
        "uploads/*",
    }, puts


def test_backend_runs_uploads_only_under_uploads(default_stacks: Stacks) -> None:
    api = default_stacks["api"]
    t = Template.from_stack(api)
    statements = [
        st
        for st in _statements_for(t, api, api.backend_lambda.role)
        if "s3:AbortMultipartUpload" in _actions(st)
    ]
    assert len(statements) == 1, statements
    assert set(_actions(statements[0])) == {
        "s3:PutObject",
        "s3:AbortMultipartUpload",
        "s3:ListMultipartUploadParts",
    }
    resources = _resources(statements[0])
    assert len(resources) == 1, resources
    assert _bucket_objects(resources, default_stacks["projects"][0]) == {"uploads/*"}


def test_a_created_project_bucket_takes_browser_uploads() -> None:
    app = cdk.App()
    ident = IdentityStack(app, "ident")
    proj = ProjectStack(
        app,
        "proj",
        project_id="p",
        permissions_boundary=ident.permissions_boundary,
        browser_origin="https://biffix.example.org",
    )
    assert proj.uploads_enabled
    t = Template.from_stack(proj)
    t.has_resource_properties(
        "AWS::S3::Bucket",
        Match.object_like(
            {
                "CorsConfiguration": {
                    "CorsRules": [
                        Match.object_like(
                            {
                                "AllowedMethods": ["PUT"],
                                "AllowedOrigins": ["https://biffix.example.org"],
                                "ExposedHeaders": ["ETag"],
                            }
                        ),
                        # The alignment viewer's Range reads.
                        Match.object_like(
                            {
                                "AllowedMethods": ["GET", "HEAD"],
                                "AllowedOrigins": ["https://biffix.example.org"],
                                "AllowedHeaders": ["Range"],
                            }
                        ),
                    ]
                },
                "LifecycleConfiguration": {
                    "Rules": Match.array_with(
                        [
                            Match.object_like(
                                {
                                    "Prefix": "uploads/",
                                    "AbortIncompleteMultipartUpload": {"DaysAfterInitiation": 7},
                                    "Status": "Enabled",
                                }
                            )
                        ]
                    )
                },
            }
        ),
    )


def test_an_imported_bucket_takes_uploads_only_when_its_owner_applied_cors() -> None:
    app = cdk.App()
    ident = IdentityStack(app, "ident")
    common: dict[str, Any] = {"project_id": "p", "permissions_boundary": ident.permissions_boundary}
    off = ProjectStack(app, "off", bucket_name="theirs", **common)
    on = ProjectStack(app, "on", bucket_name="theirs", uploads_cors_applied=True, **common)
    viewer = ProjectStack(app, "viewer", bucket_name="theirs", viewer_cors_applied=True, **common)
    created = ProjectStack(app, "created", **common)
    assert not off.uploads_enabled and not off.viewer_cors
    assert on.uploads_enabled and not on.viewer_cors
    assert viewer.viewer_cors and not viewer.uploads_enabled
    assert created.uploads_enabled and created.viewer_cors


def test_upload_bucket_settings_match_what_a_created_bucket_gets() -> None:
    settings = upload_bucket_settings("https://biffix.example.org")
    rule, viewer = settings["cors"]["CORSRules"]
    assert rule["AllowedOrigins"] == ["https://biffix.example.org"]
    assert rule["AllowedMethods"] == ["PUT"]
    assert rule["ExposeHeaders"] == ["ETag"]
    assert viewer == viewer_cors_rule("https://biffix.example.org")
    assert viewer["AllowedMethods"] == ["GET", "HEAD"]
    assert viewer["AllowedHeaders"] == ["Range"]
    assert viewer["ExposeHeaders"] == ["Content-Range", "Content-Length", "Accept-Ranges", "ETag"]
    life = settings["lifecycle"]["Rules"][0]
    assert life["Filter"] == {"Prefix": "uploads/"}
    assert life["AbortIncompleteMultipartUpload"] == {"DaysAfterInitiation": 7}
    # And they are what ProjectStack gives a bucket it creates (the two definitions cannot drift).
    app = cdk.App()
    ident = IdentityStack(app, "ident")
    proj = ProjectStack(
        app,
        "proj",
        project_id="p",
        permissions_boundary=ident.permissions_boundary,
        browser_origin="https://biffix.example.org",
    )
    (bucket,) = Template.from_stack(proj).find_resources("AWS::S3::Bucket").values()
    assert bucket["Properties"]["CorsConfiguration"]["CorsRules"] == [
        {
            "AllowedHeaders": r["AllowedHeaders"],
            "AllowedMethods": r["AllowedMethods"],
            "AllowedOrigins": r["AllowedOrigins"],
            "ExposedHeaders": r["ExposeHeaders"],
            "MaxAge": r["MaxAgeSeconds"],
        }
        for r in settings["cors"]["CORSRules"]
    ]
    (abort,) = [
        r
        for r in bucket["Properties"]["LifecycleConfiguration"]["Rules"]
        if r.get("Id") == life["ID"]
    ]
    assert abort == {
        "Id": life["ID"],
        "Status": life["Status"],
        "Prefix": life["Filter"]["Prefix"],
        "AbortIncompleteMultipartUpload": life["AbortIncompleteMultipartUpload"],
    }


def test_a_created_project_bucket_expires_superseded_versions(default_stacks: Stacks) -> None:
    # The bucket is versioned, so the intermediates and cache sweeps' key-level deletes only add
    # delete markers: a bucket-wide noncurrent-version expiration (plus orphaned delete-marker
    # cleanup) is what frees the space, after `retention.noncurrent_versions_days`.
    def rules(days: int | None | Literal["default"] = "default") -> list[Json]:
        """The created bucket's lifecycle rules, with `noncurrent_versions_days=days` (the
        stack's default when "default")."""
        app = cdk.App()
        boundary = IdentityStack(app, "ident").permissions_boundary
        proj = (
            ProjectStack(app, "proj", project_id="p", permissions_boundary=boundary)
            if days == "default"
            else ProjectStack(
                app,
                "proj",
                project_id="p",
                permissions_boundary=boundary,
                noncurrent_versions_days=days,
            )
        )
        buckets = Template.from_stack(proj).find_resources("AWS::S3::Bucket").values()
        return [r for b in buckets for r in b["Properties"]["LifecycleConfiguration"]["Rules"]]

    rule = noncurrent_versions_lifecycle_rule(30)
    (expire,) = [r for r in rules() if r["Id"] == rule["ID"]]
    assert expire == {
        "Id": rule["ID"],
        "Status": "Enabled",
        "NoncurrentVersionExpiration": rule["NoncurrentVersionExpiration"],
        "ExpiredObjectDeleteMarker": True,
    }, expire
    assert "Prefix" not in expire  # bucket-wide: no prefix can select `runs/*/work/`
    (expire,) = [r for r in rules(7) if r["Id"] == rule["ID"]]
    assert expire["NoncurrentVersionExpiration"] == {"NoncurrentDays": 7}
    assert not [r for r in rules(None) if r["Id"] == rule["ID"]]
    # The default graph wires the config's window (30 days by default) into every project bucket.
    # (test_config_keys covers how the loader reads the window.)
    assert CONFIG.noncurrent_versions_days == 30
    for project in default_stacks["projects"]:
        (bucket,) = Template.from_stack(project).find_resources("AWS::S3::Bucket").values()
        (expire,) = [
            r
            for r in bucket["Properties"]["LifecycleConfiguration"]["Rules"]
            if r["Id"] == rule["ID"]
        ]
        assert expire["NoncurrentVersionExpiration"] == {"NoncurrentDays": 30}, project


def _default_context(stacks: Stacks) -> Json:
    """The default project's context, as the backend reads it."""
    api = stacks["api"]
    return _project_context(api, api.default_project_id)


def test_backend_is_told_whether_uploads_are_on(default_stacks: Stacks, tmp_path: Path) -> None:
    def uploads_enabled(stacks: Stacks) -> bool:
        enabled = _default_context(stacks)["uploads"]
        assert isinstance(enabled, bool), enabled
        return enabled

    # A created bucket takes uploads, from the public origin or (without one) the local dev UI.
    assert CONFIG.public_base_url == "http://localhost:5173"
    assert uploads_enabled(default_stacks) is True
    public = dataclasses.replace(CONFIG, app_domain="biffix.example.org")
    assert public.public_base_url == "https://biffix.example.org"
    (tmp_path / "public").mkdir()
    assert uploads_enabled(_build(tmp_path / "public", config=public)) is True
    # An imported bucket whose owner has not applied the CORS rule does not, and the backend is
    # not granted uploads into it.
    imported = dataclasses.replace(CONFIG, projects=[Project(id="demo", bucket_name="theirs")])
    (tmp_path / "imported").mkdir()
    stacks = _build(tmp_path / "imported", config=imported)
    assert uploads_enabled(stacks) is False
    api = stacks["api"]
    statements = _statements_for(Template.from_stack(api), api, api.backend_lambda.role)
    # Wildcards included, in the actions and the resources: no statement may put an upload there.
    upload = "arn:aws:s3:::theirs/uploads/x"
    assert not [
        st
        for st in statements
        if any(_grants(st, a) for a in ("s3:AbortMultipartUpload", "s3:PutObject"))
        and any(fnmatch.fnmatchcase(upload, r) for r in _resources(st))
    ]


def test_backend_is_told_whether_the_viewer_can_read_the_buckets(
    default_stacks: Stacks, tmp_path: Path
) -> None:
    # Each project's flag is explicit in its context (a created bucket has the viewer's rule).
    assert _default_context(default_stacks)["viewer_cors"] is True

    def build(name: str, project: Project) -> Json:
        (tmp_path / name).mkdir()
        stacks = _build(tmp_path / name, config=dataclasses.replace(CONFIG, projects=[project]))
        return _default_context(stacks)

    # An imported bucket only when its owner applied it; a data source carries its own flag.
    source = DataSource(id="refs", name="Refs", bucket="shared-refs", viewer_cors_applied=True)
    neither = build("neither", Project(id="demo", bucket_name="theirs", data_sources=(source,)))
    assert (neither["uploads"], neither["viewer_cors"]) == (False, False)
    (entry,) = neither["data_sources"]
    assert entry["viewer_cors"] is True
    uploads_only = build(
        "uploads", Project(id="demo", bucket_name="theirs", uploads_cors_applied=True)
    )
    assert (uploads_only["uploads"], uploads_only["viewer_cors"]) == (True, False)
    viewer_only = build(
        "viewer", Project(id="demo", bucket_name="theirs", viewer_cors_applied=True)
    )
    assert (viewer_only["uploads"], viewer_only["viewer_cors"]) == (False, True)


def _cors_of(instruction: str) -> tuple[str, Json]:
    """The bucket and CORS configuration of the `put-bucket-cors` line in `instruction`."""
    (line,) = [
        ln for ln in instruction.splitlines() if ln.startswith("  aws s3api put-bucket-cors ")
    ]
    bucket = line.split("--bucket ")[1].split()[0]
    return bucket, json.loads(line.split("--cors-configuration '")[1].rstrip("'"))


def test_the_deploy_prints_one_cors_command_per_bucket_it_does_not_own() -> None:
    origin = "https://biffix.example.org"
    base = dataclasses.replace(CONFIG, app_domain="biffix.example.org")
    assert base.public_base_url == origin

    def instructions(*projects: Project) -> list[str]:
        return bucket_cors_instructions(dataclasses.replace(base, projects=list(projects)))

    # A created bucket (and its data sources in it) need nothing; nor a source with the rule.
    assert instructions(Project(id="p")) == []
    done = DataSource(id="d", name="D", bucket="refs", viewer_cors_applied=True)
    assert instructions(Project(id="p", data_sources=(done,))) == []

    # An imported bucket with uploads on but no viewer rule: ONE command carrying both rules (it
    # replaces the bucket's rules), no lifecycle, and both flags to set.
    in_bucket = DataSource(id="refs", name="Refs", bucket="theirs", prefix="references/")
    (only,) = instructions(
        Project(id="p", bucket_name="theirs", uploads_cors_applied=True, data_sources=(in_bucket,))
    )
    assert _cors_of(only) == ("theirs", upload_bucket_settings(origin)["cors"])
    assert [r["AllowedMethods"] for r in _cors_of(only)[1]["CORSRules"]] == [
        ["PUT"],
        ["GET", "HEAD"],
    ]
    assert "lifecycle" not in only
    assert "the alignment viewer is off" in only
    assert (
        "REPLACES the bucket's CORS rules; this one includes the upload rule; merge any other "
        "rules the bucket has first" in only
    )
    assert "`uploads_cors_applied: true` and `viewer_cors_applied: true`" in only

    # Neither rule: the same CORS command, plus the uploads lifecycle rule.
    (both,) = instructions(Project(id="p", bucket_name="theirs"))
    assert _cors_of(both) == ("theirs", upload_bucket_settings(origin)["cors"])
    assert "put-bucket-lifecycle-configuration --bucket theirs" in both
    assert "browser uploads and the alignment viewer are off" in both
    # Both applied: nothing.
    assert (
        instructions(
            Project(
                id="p", bucket_name="theirs", uploads_cors_applied=True, viewer_cors_applied=True
            )
        )
        == []
    )

    # A data source's own bucket: a GET/HEAD-only command, once per bucket however many sources
    # (and projects) read from it; a project bucket's sources are covered by its command.
    shared = [
        DataSource(id="a", name="A", bucket="shared-refs", prefix="hg38/"),
        DataSource(id="b", name="B", bucket="shared-refs", prefix="mm39/"),
    ]
    out = instructions(
        Project(
            id="p",
            bucket_name="theirs",
            uploads_cors_applied=True,
            viewer_cors_applied=True,
            data_sources=(shared[0], in_bucket),
        ),
        Project(id="q", data_sources=(shared[1],)),
    )
    (source,) = out
    assert _cors_of(source) == ("shared-refs", {"CORSRules": [viewer_cors_rule(origin)]})
    assert "`p/a`, `q/b`" in source
    assert "`viewer_cors_applied: true` on the data source" in source
    assert "merge any other rules the bucket has first" in source

    # The rule is the bucket's: one source flagging it covers the others in that bucket (as the
    # backend reads the flags), so nothing is printed for it.
    flagged = dataclasses.replace(shared[1], viewer_cors_applied=True)
    assert (
        instructions(
            Project(id="p", data_sources=(shared[0],)), Project(id="q", data_sources=(flagged,))
        )
        == []
    )


def test_the_deploy_prints_the_combined_rule_for_an_imported_bucket_with_uploads() -> None:
    # The imported bucket already has the upload (PUT) rule; put-bucket-cors replaces a bucket's
    # rules, so what is printed must carry it too.
    config = dataclasses.replace(
        CONFIG,
        app_domain="biffix.example.org",
        projects=[
            Project(
                id="genomics",
                bucket_name="acme-genomics-data",
                uploads_cors_applied=True,
                data_sources=(DataSource(id="refs", name="R", bucket="acme-genomics-data"),),
            )
        ],
    )
    (only,) = bucket_cors_instructions(config)
    bucket, cors = _cors_of(only)
    assert bucket == "acme-genomics-data"
    assert [r["AllowedMethods"] for r in cors["CORSRules"]] == [["PUT"], ["GET", "HEAD"]]
