//! A workflow checkout on this machine, as `register`, `workflows form` and `inspect` read it: its
//! git source and the files it ships ([`source`]), its manifest and launch form ([`form`]), the
//! edits `inspect --write` makes to the manifest ([`manifest_edit`]), its entrypoint
//! ([`entrypoint`]), its Nextflow plugins ([`nf_plugins`]) and the Snakemake memory advisory
//! ([`memory_warning`]). Nothing here talks to a deployment.

pub(crate) mod entrypoint;
pub(crate) mod form;
pub(crate) mod manifest_edit;
pub(crate) mod memory_warning;
pub(crate) mod nf_plugins;
pub(crate) mod source;
