//! The pin tables: the exact versions and sha256 sums each supported engine release's runtime is
//! built from. A version bump is an edit here (and the tests in `runtime/tests.rs` that tie these
//! pins to the plugins and the coordinator image).

use crate::runtime::{
    GitSource, NextflowPins, NextflowRelease, PluginPin, PythonBuild, SnakemakePins, UvBuild,
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
        // 0.3.6 plus upstream PR #68 (snakemake/snakemake-storage-plugin-s3: filesystem
        // modification times kept in S3 object metadata, so an S3 clock ahead of the host cannot
        // make a fresh output look older than its input), from the fork's never-force-pushed
        // branch `biffix-pin/2026-11-20`; re-pin to the release that ships it.
        storage_s3: "0.3.6+g8129cca56f7c".into(),
        storage_s3_source: Some(GitSource {
            repo: "https://github.com/nh13/snakemake-storage-plugin-s3.git".into(),
            sha: "8129cca56f7cea57b1a40c942705036c9e5fc826".into(),
            // Its `poetry-core` build backend (unconstrained in the checkout's pyproject), the
            // release current at `exclude_newer`, by the sha256 of its PyPI wheel.
            build_requires: vec![
                "poetry-core==2.5.0@98ee465b66f7e14bb9fbe68fb5e00d0cff184f036993ec94610ae4b5612f3add"
                    .into(),
            ],
        }),
        executor_sha: "0ac5ec4ea45a0c278876a598b17327213e964483".into(),
        logger_version: "0.2.4".into(),
        single_job_executor_version: Some("0.1.0".into()),
        exclude_newer: "2026-09-26T00:00:00Z".into(),
    }
}

/// The Nextflow pin table, for `release` (spelled as the engine writes it); `None` for a release
/// whose downloads are not pinned here, since biffix never installs an unverified launcher or
/// plugin. A release is added with the sha256 of its `-dist` asset and of the plugin zips its
/// `plugins-info.txt` names (`registry.nextflow.io/api/v1/plugins/<id>/<version>/download/…`).
///
/// One nf-biffix serves every release: it builds against the oldest (25.10.4, nf-amazon 3.4.4), and
/// the nf-amazon API it extends is unchanged through 26.10.
pub(super) fn nextflow(release: &str) -> Option<NextflowPins> {
    let plugin =
        |version: &str, sha256: &str| PluginPin { version: version.into(), sha256: sha256.into() };
    let downloads = |launcher_sha256: &str, nf_amazon, nf_cloudcache| NextflowRelease {
        launcher_sha256: launcher_sha256.into(),
        nf_amazon,
        nf_cloudcache,
    };
    let release = match release {
        "25.10.4" => downloads(
            "60db869a7618783e3ee60183a2b0c6c50ffc67221eabec60800b904991786aaa",
            plugin("3.4.4", "677f69b82f63a43cb5809eb61d7045846a10af394d29168211c9a3c74fa39ac7"),
            plugin("0.5.0", "39afd45d1dcc2765ce77a0ee429559f2cf1ff52c27347f2a94b2f35a05b46775"),
        ),
        "26.04.9" => downloads(
            "240befc5fc206b76bdbed75fb3f34ca43ad822e7dfd1649474491cdc54e107be",
            plugin("3.9.3", "e567cb638e2844db61ccd26f10cf95b9a571daeb2c3cc32ae238552eca3c8357"),
            plugin("0.6.0", "852a979adac1b57e47a09ceeacd8bf525d6747a33383f1c96ff02d150d326860"),
        ),
        "26.10.5" => downloads(
            "b030030c06040bf5f13ac423c318d146c95e8d1d82f5022799dcb24738bccf3e",
            plugin("3.12.1", "ac12f6c517b54e1955fe5ccdef64ebe8ae9023991cf4c0c8e42d50ffc84e002e"),
            plugin("0.6.2", "14d4df81b76074c51979b9a24ce5746b9a7c23825fb6807fb020c9404bb5f1b4"),
        ),
        _ => return None,
    };
    Some(NextflowPins { nf_biffix_version: "0.8.1".into(), release, workflow_plugins: Vec::new() })
}
