# biffix

The command-line client for [biffix](https://github.com/nh13/biffix), a self-hostable control plane
for running and tracking Snakemake and Nextflow workflows on AWS Batch. It signs in to a biffix
deployment, registers and releases workflow versions, and submits and follows runs.

```sh
cargo install --locked biffix
```

Sign in once per deployment (its address followed by `/api`), then use it:

```sh
biffix login --url https://biffix.example.org/api
biffix workflows register . --project rnaseq
biffix runs submit <version-id> --params params.yaml --follow
biffix runs list
```

`biffix --help` lists every command. The
[user guide](https://github.com/nh13/biffix/blob/main/docs/user-guide.md) covers profiles,
authoring a workflow and launching runs.

The CLI needs a biffix deployment to talk to; see the
[deployment guide](https://github.com/nh13/biffix/blob/main/docs/deploying.md) to set one up.
