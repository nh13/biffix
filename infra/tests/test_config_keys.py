"""The deploy config says only what the deploy does: unknown keys and unhonoured values fail, and
what it derives from the values it reads. (What the stacks make of it is in tests/synth/.)"""

import re
from pathlib import Path

import pytest

from biffix_infra.config import DeployConfig, HostAwsCli, QueueClass
from biffix_infra.config.fields import Raw
from biffix_infra.rbac import RBAC_GROUP_ROLES
from tests.deploy_config_file import FIXTURE, write_config

# (The documented example, infra/biffix-deploy.example.yaml, is checked in test_example_config.py.)


def _load(tmp_path: Path, *, raw: str = "", **overrides: object) -> DeployConfig:
    return DeployConfig.load(write_config(tmp_path, raw=raw, **overrides))


@pytest.mark.parametrize(
    ("overrides", "where"),
    [
        ({"nat_modes": "gateway"}, "nat_modes"),
        ({"retention": {"intermediate_days": 7}}, "intermediate_days"),
        (
            {"queue_classes": [{"name": "coordinator", "coordinator": True, "ebs_size": 500}]},
            "ebs_size",
        ),
        ({"projects": [{"id": "demo", "bucket": "b"}]}, "bucket"),
        (
            {
                "projects": [
                    {
                        "id": "demo",
                        "data_sources": [{"id": "a", "name": "A", "bucket": "b", "prefx": "p/"}],
                    }
                ]
            },
            "prefx",
        ),
        ({"image_archive": {"deep_archive_days": 90}}, "deep_archive_days"),
        ({"image_archive": {"object_lock": {"mode": "GOVERNANCE", "days": 1, "x": 1}}}, "x"),
        (
            {"host_aws_cli": {"version": "2", "sha256_x86_64": "a", "sha256_aarch64": "b", "v": 1}},
            "v",
        ),
        ({"registration": {"github_subject": ["repo:a/b:ref:refs/heads/main"]}}, "github_subject"),
        ({"registration": {"biffix_image_build": {"subject": "s", "branch": "b"}}}, "branch"),
        ({"authorization": {"default_role": "viewer"}}, "default_role"),
        (
            {"google_oidc": {"client_id": "c", "client_secret": "s"}, "cognito_domain_prefix": "p"},
            "client_secret",
        ),
    ],
)
def test_an_unknown_key_is_rejected(tmp_path: Path, overrides: Raw, where: str) -> None:
    # The rejected key itself (each row plants exactly one), not a word of the allowed-keys list
    # that follows it: `x` is in "expected", `bucket` in "bucket_name".
    with pytest.raises(ValueError, match=rf"unknown key\(s\) {re.escape(where)};"):
        _load(tmp_path, **overrides)


def test_ebs_tuning_applies_to_the_default_root_volume_too(tmp_path: Path) -> None:
    # Every host gets a gp3 root volume (30 GB unless `ebs_gb` sizes it), so its speed can be
    # tuned without sizing it.
    for knob, value in (("ebs_iops", 4000), ("ebs_throughput", 250)):
        cfg = _load(
            tmp_path, queue_classes=[{"name": "coordinator", "coordinator": True, knob: value}]
        )
        assert getattr(cfg.queue_classes[0], knob) == value


@pytest.mark.parametrize(
    ("queue_class", "match"),
    [
        ({"ebs_gb": 0}, "ebs_gb"),
        ({"ebs_gb": "100"}, "ebs_gb"),
        ({"ebs_gb": True}, "ebs_gb"),
        ({"ebs_iops": 2999}, "ebs_iops"),
        ({"ebs_iops": 16001}, "ebs_iops"),
        # gp3 allows at most 500 iops per GiB: 15000 on the default 30 GB volume.
        ({"ebs_iops": 15001}, "ebs_iops .*500 per GiB"),
        ({"ebs_gb": 10, "ebs_iops": 5001}, "ebs_iops .*500 per GiB"),
        ({"ebs_throughput": 124}, "ebs_throughput"),
        ({"ebs_throughput": 1001}, "ebs_throughput"),
        ({"imds_hop_limit": 0}, "imds_hop_limit"),
        ({"imds_hop_limit": 65}, "imds_hop_limit"),
        ({"imds_hop_limit": True}, "imds_hop_limit"),
    ],
)
def test_a_queue_class_s_volume_and_metadata_settings_are_in_range(
    tmp_path: Path, queue_class: Raw, match: str
) -> None:
    with pytest.raises(ValueError, match=match):
        _load(
            tmp_path,
            queue_classes=[{"name": "coordinator", "coordinator": True, **queue_class}],
        )


@pytest.mark.parametrize(
    ("overrides", "match"),
    [
        # `true` is not one day.
        ({"image_archive": {"object_lock": {"mode": "GOVERNANCE", "days": True}}}, "object_lock"),
        ({"image_archive": {"object_lock": {"mode": "GOVERNANCE", "days": 0}}}, "object_lock"),
        ({"image_archive": {"instant_retrieval_after_days": "0"}}, "instant_retrieval"),
        ({"image_archive": {"deep_archive_after_days": -1}}, "deep_archive"),
        ({"aurora_backup_retention_days": 0}, "aurora_backup_retention_days"),
        ({"aurora_backup_retention_days": 36}, "aurora_backup_retention_days"),
        ({"aurora_backup_retention_days": None}, "aurora_backup_retention_days"),
        ({"aurora_backup_retention_days": "14"}, "aurora_backup_retention_days"),
    ],
)
def test_day_counts_are_whole_numbers_in_range(tmp_path: Path, overrides: Raw, match: str) -> None:
    with pytest.raises(ValueError, match=match):
        _load(tmp_path, **overrides)


def test_the_default_role_set_is_rbac_s(tmp_path: Path) -> None:
    for role in set(RBAC_GROUP_ROLES.values()):
        cfg = _load(tmp_path, authorization={"authenticated_default_role": role})
        assert cfg.authenticated_default_role == role
    with pytest.raises(ValueError, match="authenticated_default_role"):
        _load(tmp_path, authorization={"authenticated_default_role": "root"})


def test_min_tiered_object_bytes_is_a_byte_count(tmp_path: Path) -> None:
    for bad in (-1, "1MiB", True):
        with pytest.raises(ValueError, match="min_tiered_object_bytes"):
            _load(tmp_path, image_archive={"min_tiered_object_bytes": bad})
    assert _load(tmp_path, image_archive={"min_tiered_object_bytes": 0}).image_archive


def test_worker_bucket_lists(tmp_path: Path) -> None:
    cfg = _load(
        tmp_path,
        projects=[
            {
                "id": "demo",
                "bucket_name": "existing",
                "data_sources": [
                    {"id": "a", "name": "A", "bucket": "core-drop"},
                    {"id": "b", "name": "B", "bucket": "core-drop", "prefix": "x"},
                ],
            },
            {"id": "other", "data_sources": [{"id": "r", "name": "R", "bucket": "refs"}]},
        ],
    )
    assert cfg.data_source_buckets == ["core-drop", "refs"]
    assert cfg.imported_project_buckets == ["existing"]


def test_the_example_s_coordinator_queue_class_is_on_demand() -> None:
    qc = DeployConfig.load(FIXTURE).coordinator_queue_class
    assert qc.coordinator is True
    assert qc.capacity == "ON_DEMAND"  # coordinator is never spot


def test_coordinator_queue_class_requires_exactly_one() -> None:
    zero = DeployConfig(
        tenant="dev",
        region="us-east-1",
        projects=[],
        queue_classes=[QueueClass(name="cpu", capacity="SPOT")],
    )
    with pytest.raises(ValueError, match="no coordinator"):
        _ = zero.coordinator_queue_class
    two = DeployConfig(
        tenant="dev",
        region="us-east-1",
        projects=[],
        queue_classes=[
            QueueClass(name="c1", capacity="ON_DEMAND", coordinator=True),
            QueueClass(name="c2", capacity="ON_DEMAND", coordinator=True),
        ],
    )
    with pytest.raises(ValueError, match="exactly one coordinator"):
        _ = two.coordinator_queue_class


def test_config_rejects_bad_nat_mode_and_worker_egress(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="nat_mode"):
        _load(tmp_path, nat_mode="banana")
    with pytest.raises(ValueError, match="worker_egress"):
        _load(tmp_path, worker_egress="closed")


def test_config_rejects_bad_queue_class_capacity(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="capacity"):
        _load(
            tmp_path,
            queue_classes=[
                {"name": "coordinator", "capacity": "ON_DEMAND", "coordinator": True},
                {"name": "cpu", "capacity": "CHEAP"},
            ],
        )


def test_a_queue_class_s_max_vcpus_is_a_positive_count(tmp_path: Path) -> None:
    for bad in (0, "64", True):
        with pytest.raises(ValueError, match="max_vcpus"):
            _load(
                tmp_path,
                queue_classes=[
                    {"name": "coordinator", "capacity": "ON_DEMAND", "coordinator": True},
                    {"name": "bench", "max_vcpus": bad},
                ],
            )


@pytest.mark.parametrize(
    ("overrides", "match"),
    [
        # A block given as another falsy value is the wrong shape, not its defaults.
        ({"image_archive": {"object_lock": False}}, "image_archive.object_lock: expected a map"),
        ({"registration": ""}, "registration: expected a mapping"),
        ({"retention": []}, "retention: expected a mapping"),
        # At least one project, at load (not only at synth).
        ({"projects": []}, "at least one project"),
    ],
)
def test_a_block_of_the_wrong_shape_names_itself(
    tmp_path: Path, overrides: Raw, match: str
) -> None:
    with pytest.raises(ValueError, match=match):
        _load(tmp_path, **overrides)


def test_an_unknown_key_in_a_nested_list_names_its_parent(tmp_path: Path) -> None:
    sources = [{"id": "a", "name": "A", "bucket": "b", "prefx": "p/"}]
    with pytest.raises(ValueError, match="project 'p'.*data source 'a'.*prefx"):
        _load(tmp_path, projects=[{"id": "p", "data_sources": sources}])


def test_config_present_but_null_blocks_do_not_raise_attributeerror(tmp_path: Path) -> None:
    # A present-but-null `registration:`/`authorization:` block must degrade to the safe defaults,
    # not blow up with a raw AttributeError (which `raw.get(k, {}).get(...)` would).
    cfg = _load(tmp_path, registration=None, authorization=None)
    assert cfg.github_subjects == []
    assert cfg.image_build_trust is None
    assert cfg.authenticated_default_role == "viewer"


@pytest.mark.parametrize(
    ("registration", "match"),
    [
        # A string is not split into characters, whose `*` would trust every repository.
        ({"github_subjects": "repo:acme/pipelines:*"}, "github_subjects .*expected a list"),
        ({"github_subjects": ["*"]}, r"github_subjects entry '\*'"),
        ({"github_subjects": [""]}, "github_subjects entry ''"),
        ({"github_subjects": [None]}, "github_subjects entry None"),
        # A wildcard in the repository itself would trust other repositories.
        ({"github_subjects": ["repo:acme/*:ref:refs/heads/main"]}, "github_subjects entry"),
        ({"github_subjects": ["repo:*/pipelines:*"]}, "github_subjects entry"),
        # A null `ref` would drop the branch pin from the trust policy.
        ({"biffix_image_build": {"subject": "repo:a/b:environment:e", "ref": None}}, "ref"),
        ({"biffix_image_build": {"subject": "repo:a/b:environment:e", "ref": ""}}, "ref"),
        ({"biffix_image_build": {"subject": "repo:a/b:environment:e", "ref": "main"}}, "ref"),
        ({"biffix_image_build": {"subject": ["repo:a/b:environment:e"]}}, "subject"),
        ({"biffix_image_build": {"subject": "repo:a/*:environment:e"}}, "subject"),
    ],
)
def test_registration_trust_is_never_wider_than_written(
    tmp_path: Path, registration: Raw, match: str
) -> None:
    # These values become GitHub OIDC trust conditions: a malformed one must fail the load, never
    # quietly widen who may assume the role.
    with pytest.raises(ValueError, match=match):
        _load(tmp_path, registration=registration)


def test_registration_accepts_subjects_scoped_to_a_repository(tmp_path: Path) -> None:
    subjects = ["repo:acme/pipelines:ref:refs/heads/main", "repo:acme@1/other@2:*"]
    cfg = _load(
        tmp_path,
        registration={
            "github_subjects": subjects,
            "biffix_image_build": {"subject": "repo:acme/biffix:environment:image-build"},
        },
    )
    assert cfg.github_subjects == subjects
    assert cfg.image_build_trust is not None
    assert cfg.image_build_trust.ref == "refs/heads/main"


def test_config_certificate_arn_requires_app_domain(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="app_certificate_arn requires app_domain"):
        _load(tmp_path, app_certificate_arn="arn:aws:acm:us-east-1:1:certificate/x")


def test_config_google_oidc_requires_domain_prefix(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="cognito_domain_prefix"):
        _load(tmp_path, google_oidc={"client_id": "abc"})


def test_config_google_secret_name_is_derived_from_tenant(tmp_path: Path) -> None:
    # A derived convention (deploy.sh and IdentityStack must agree); None when federation is off.
    assert _load(tmp_path).google_secret_name is None
    on = _load(tmp_path, google_oidc={"client_id": "abc"}, cognito_domain_prefix="biffix-x")
    assert on.google_secret_name == f"biffix/{on.tenant}/google-oidc"


def test_config_cognito_mfa_defaults_to_optional(tmp_path: Path) -> None:
    assert DeployConfig.load(FIXTURE).cognito_mfa == "optional"
    assert _load(tmp_path, cognito_mfa="required").cognito_mfa == "required"
    with pytest.raises(ValueError, match="cognito_mfa"):
        _load(tmp_path, cognito_mfa="sms")
    # The documented spelling, unquoted, which YAML reads as false.
    assert _load(tmp_path, raw="cognito_mfa: off\n").cognito_mfa == "off"
    with pytest.raises(ValueError, match="cognito_mfa True"):
        _load(tmp_path, raw="cognito_mfa: on\n")


def test_config_host_aws_cli_defaults_and_requires_both_checksums(tmp_path: Path) -> None:
    assert DeployConfig.load(FIXTURE).host_aws_cli == HostAwsCli()
    pinned = {"version": "2.99.0", "sha256_x86_64": "a" * 64, "sha256_aarch64": "b" * 64}
    cfg = _load(tmp_path, host_aws_cli=pinned)
    assert cfg.host_aws_cli == HostAwsCli("2.99.0", "a" * 64, "b" * 64)
    assert cfg.host_aws_cli.key("aarch64") == "host/aws-cli/2.99.0/aarch64.zip"
    with pytest.raises(ValueError, match="sha256_aarch64"):
        _load(tmp_path, host_aws_cli={"version": "2.99.0", "sha256_x86_64": "a" * 64})


def test_config_retention_defaults_to_30_days_and_null_keeps_forever(tmp_path: Path) -> None:
    assert _load(tmp_path).intermediates_retention_days == 30
    assert (
        _load(tmp_path, retention={"intermediates_days": None}).intermediates_retention_days is None
    )
    assert _load(tmp_path, retention={"intermediates_days": 7}).intermediates_retention_days == 7
    for bad in (-1, "30", True):
        with pytest.raises(ValueError, match="intermediates_days"):
            _load(tmp_path, retention={"intermediates_days": bad})


def test_config_noncurrent_versions_days_defaults_to_30_and_null_keeps_them(tmp_path: Path) -> None:
    assert _load(tmp_path).noncurrent_versions_days == 30
    cfg = _load(tmp_path, retention={"noncurrent_versions_days": None})
    assert cfg.noncurrent_versions_days is None
    for bad in (0, "30", True):
        with pytest.raises(ValueError, match="noncurrent_versions_days"):
            _load(tmp_path, retention={"noncurrent_versions_days": bad})


def test_config_parses_data_sources(tmp_path: Path) -> None:
    cfg = _load(
        tmp_path,
        projects=[
            {
                "id": "p",
                "data_sources": [
                    {"id": "core", "name": "Core", "bucket": "b", "prefix": "/drop/x"},
                    {"id": "refs", "name": "Refs", "bucket": "r", "kind": "references"},
                ],
            }
        ],
    )
    core, refs = cfg.projects[0].data_sources
    assert (core.prefix, core.kind) == ("drop/x/", "data")
    assert (refs.prefix, refs.kind) == ("", "references")
    assert core.to_backend() == {
        "id": "core",
        "name": "Core",
        "bucket": "b",
        "prefix": "drop/x/",
        "kind": "data",
    }
    assert not core.viewer_cors_applied


def test_config_reads_the_cors_flags_of_buckets_biffix_does_not_own(tmp_path: Path) -> None:
    cfg = _load(
        tmp_path,
        projects=[
            {
                "id": "p",
                "bucket_name": "theirs",
                "uploads_cors_applied": True,
                "viewer_cors_applied": True,
                "data_sources": [
                    {"id": "refs", "name": "Refs", "bucket": "r", "viewer_cors_applied": True}
                ],
            },
            {"id": "q", "bucket_name": "other"},
        ],
    )
    p, q = cfg.projects
    assert (p.uploads_cors_applied, p.viewer_cors_applied) == (True, True)
    assert (q.uploads_cors_applied, q.viewer_cors_applied) == (False, False)
    (refs,) = p.data_sources
    assert refs.viewer_cors_applied
    # The backend reads the source's flag as `viewer_cors`.
    assert refs.to_backend()["viewer_cors"] is True


def test_a_null_value_means_the_key_s_default(tmp_path: Path) -> None:
    # `prefix:` left empty is the whole bucket, not a folder named `None`.
    cfg = _load(
        tmp_path,
        projects=[
            {
                "id": "p",
                "key_arn": None,
                "uploads_cors_applied": None,
                "data_sources": [
                    {
                        "id": "a",
                        "name": "A",
                        "bucket": "b",
                        "prefix": None,
                        "description": None,
                        "kind": None,
                        "viewer_cors_applied": None,
                    }
                ],
            }
        ],
    )
    (project,) = cfg.projects
    assert (project.key_arn, project.uploads_cors_applied) == (None, False)
    (source,) = project.data_sources
    assert (source.prefix, source.kind, source.description) == ("", "data", None)
    assert not source.viewer_cors_applied


@pytest.mark.parametrize(
    ("project", "match"),
    [
        # A quoted "false" is not false: it would offer uploads on a bucket with no CORS rule.
        ({"uploads_cors_applied": "false"}, "uploads_cors_applied 'false'"),
        ({"viewer_cors_applied": "no"}, "viewer_cors_applied 'no'"),
        ({"key_arn": 1}, "key_arn 1"),
        ({"bucket_name": ["b"]}, "bucket_name"),
        ({"data_sources": [{"id": "a", "name": "A", "bucket": "b", "prefix": 7}]}, "prefix 7"),
        ({"data_sources": [{"id": 1, "name": "A", "bucket": "b"}]}, "id 1"),
        ({"data_sources": [{"id": "a", "name": 2, "bucket": "b"}]}, "name 2"),
        ({"data_sources": [{"id": "a", "name": "A", "bucket": "b", "description": 3}]}, "desc"),
        (
            {"data_sources": [{"id": "a", "name": "A", "bucket": "b", "viewer_cors_applied": 1}]},
            "viewer_cors_applied 1",
        ),
    ],
)
def test_project_values_are_never_coerced(tmp_path: Path, project: Raw, match: str) -> None:
    with pytest.raises(ValueError, match=match):
        _load(tmp_path, projects=[{"id": "p", **project}])


@pytest.mark.parametrize(
    ("overrides", "match"),
    [
        ({"coordinator_image_tag": 1.10}, "coordinator_image_tag"),
        (
            {"host_aws_cli": {"version": 2.30, "sha256_x86_64": "a", "sha256_aarch64": "b"}},
            "host_aws_cli.version",
        ),
        ({"reference_key_arn": 5}, "reference_key_arn"),
        ({"image_archive": {"key_arn": True}}, "image_archive.key_arn"),
    ],
)
def test_a_number_is_not_read_as_a_name(tmp_path: Path, overrides: Raw, match: str) -> None:
    # An unquoted `1.10` is the float 1.1: as a name it would silently be another one.
    with pytest.raises(ValueError, match=match):
        _load(tmp_path, **overrides)


@pytest.mark.parametrize(
    ("queue_class", "match"),
    [
        # A quoted "false" would make a second coordinator queue-class.
        ({"name": "cpu", "coordinator": "false"}, "coordinator 'false'"),
        ({"name": "cpu", "instances": "c"}, "instances 'c'"),
        ({"name": 7}, "queue-class name 7"),
    ],
)
def test_queue_class_values_are_never_coerced(tmp_path: Path, queue_class: Raw, match: str) -> None:
    coordinator = {"name": "coordinator", "capacity": "ON_DEMAND", "coordinator": True}
    with pytest.raises(ValueError, match=match):
        _load(tmp_path, queue_classes=[coordinator, queue_class])


def test_instance_families_map_names_to_names(tmp_path: Path) -> None:
    assert _load(tmp_path, instance_families={"c": "c7i"}).instance_families["c"] == "c7i"
    with pytest.raises(ValueError, match="instance_families.c"):
        _load(tmp_path, instance_families={"c": 7})


@pytest.mark.parametrize(
    "sources, match",
    [
        ([{"id": "project", "name": "x", "bucket": "b"}], "reserved"),
        ([{"id": "a", "name": "x"}], "bucket"),
        ([{"id": "a", "name": "x", "bucket": "b", "kind": "odd"}], "kind"),
        (
            [{"id": "a", "name": "x", "bucket": "b"}, {"id": "a", "name": "y", "bucket": "c"}],
            "duplicate",
        ),
    ],
)
def test_config_rejects_bad_data_sources(tmp_path: Path, sources: list[Raw], match: str) -> None:
    with pytest.raises(ValueError, match=match):
        _load(tmp_path, projects=[{"id": "p", "data_sources": sources}])


@pytest.mark.parametrize(
    "tenant",
    [
        # Generated bucket and repository names are lowercase, so a mixed-case tenant matches
        # none of its own resources by name.
        "Acme",
        "a_b",
        "a.b",
        "",
        "-acme",
        "biffix-acme",
        "a" * 33,
        7,
        None,
    ],
)
def test_the_tenant_is_a_short_lowercase_name(tmp_path: Path, tenant: object) -> None:
    with pytest.raises(ValueError, match="tenant"):
        _load(tmp_path, tenant=tenant)


def test_the_alarm_prefix_is_the_tenant_s_alone(tmp_path: Path) -> None:
    """The status probe lists alarms by this prefix: a sibling tenant `<tenant>-<x>`'s must not
    start with it (a bare `biffix-<tenant>-` would)."""
    assert _load(tmp_path).alarm_prefix == "biffix-x/"
    sibling = _load(tmp_path, tenant="x-dev").alarm_prefix
    assert not sibling.startswith(_load(tmp_path).alarm_prefix), sibling


@pytest.mark.parametrize(
    ("projects", "match"),
    [
        # Each project is its own isolation boundary, and the backend routes a run by its bucket.
        (
            [{"id": "a", "bucket_name": "shared"}, {"id": "b", "bucket_name": "shared"}],
            "may not share a bucket .*: shared is used by a, b",
        ),
        ([{"id": "a"}, {"id": "a"}], r"duplicate project ids: \['a'\]"),
        # An id names its SSM parameter (`/biffix/<tenant>/projects/<id>`).
        ([{"id": "a/b"}], "project id 'a/b'"),
        ([{"id": "a b"}], "project id 'a b'"),
        ([{"id": ""}], "project id ''"),
        ([{"id": "a_b"}], "project id 'a_b'"),
        ([{"id": "a.b"}], "project id 'a.b'"),
        # Its coordinator template would fall in the variants' name space
        # (`biffix-<tenant>-coordinator-rt-*`).
        ([{"id": "rt-x"}], "project id 'rt-x'.*rt-"),
    ],
)
def test_config_rejects_projects_the_backend_cannot_route(
    tmp_path: Path, projects: list[Raw], match: str
) -> None:
    with pytest.raises(ValueError, match=match):
        _load(tmp_path, projects=projects)


def test_config_accepts_distinct_projects(tmp_path: Path) -> None:
    config = _load(
        tmp_path,
        projects=[
            {"id": "genomics", "bucket_name": "acme-genomics-data"},
            {"id": "imported-v2", "bucket_name": "acme-imported-data"},
            # Created buckets are each their own.
            {"id": "c"},
            {"id": "d"},
        ],
    )
    assert [p.id for p in config.projects] == [
        "genomics",
        "imported-v2",
        "c",
        "d",
    ]
