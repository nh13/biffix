//! The pin tables: the exact versions and sha256 sums each supported engine release's runtime is
//! built from. A version bump is an edit here (and the tests in `runtime/tests.rs` that tie these
//! pins to the plugins and the coordinator image).

use crate::runtime::{
    NextflowPins, NextflowRelease, PluginPin, PythonBuild, SnakemakePins, UvBuild,
};

/// The Snakemake 9 pin table.
pub(super) fn snakemake_9() -> SnakemakePins {
    SnakemakePins {
        python: PythonBuild {
            release: "20260924".into(),
            version: "3.12.14".into(),
            sha256_x86_64: "5eae8cf79dd47fc2496a4fccc892936be831ce7a84d984b2299dfb1cdb592682"
                .into(),
            sha256_aarch64: "c0574c9c9fec561eb192ad510e4e79aaad039929b3531b3bb873e2f630fb14a9"
                .into(),
        },
        uv: UvBuild {
            version: "0.12.19".into(),
            sha256_x86_64: "23bf5552d220e0842b65c862097b2ebaeba0064b74eda5e565e77fd25969d8c8"
                .into(),
        },
        storage_s3: "0.3.6".into(),
        executor_sha: "a0aaf7a725bfb5120721413afc920f4a21737c53".into(),
        logger_version: "0.2.2".into(),
        exclude_newer: "2026-09-26T00:00:00Z".into(),
    }
}

/// The Nextflow 25 pin table, for `release` (spelled as the engine writes it); `None` for a release
/// whose downloads are not pinned here, since biffix never installs an unverified launcher or
/// plugin. A release is added with the sha256 of its `-dist` asset and of the plugin zips its
/// `plugins-info.txt` names (`registry.nextflow.io/api/v1/plugins/<id>/<version>/download/…`).
pub(super) fn nextflow_25(release: &str) -> Option<NextflowPins> {
    let plugin =
        |version: &str, sha256: &str| PluginPin { version: version.into(), sha256: sha256.into() };
    let release = match release {
        "25.10.4" => NextflowRelease {
            launcher_sha256: "60db869a7618783e3ee60183a2b0c6c50ffc67221eabec60800b904991786aaa"
                .into(),
            nf_amazon: plugin(
                "3.4.4",
                "677f69b82f63a43cb5809eb61d7045846a10af394d29168211c9a3c74fa39ac7",
            ),
            nf_cloudcache: plugin(
                "0.5.0",
                "39afd45d1dcc2765ce77a0ee429559f2cf1ff52c27347f2a94b2f35a05b46775",
            ),
        },
        _ => return None,
    };
    Some(NextflowPins { nf_biffix_version: "0.6.1".into(), release, workflow_plugins: Vec::new() })
}
