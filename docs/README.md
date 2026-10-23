# biffix documentation

biffix (say "BFX", short for bioinformatics) is a self-hostable control plane for running and
tracking Snakemake and Nextflow workflows on AWS.

- [Architecture](architecture.md): how biffix works. The event-sourced run log and its
  projections, the components, the design principles, and the compatibility rules of the
  `RunEvent` contract.
- [Deploying](deploying.md): standing up and operating a deployment. Prerequisites, the deploy
  config, deploying from a release or from source, sign-in, projects, upgrades, teardown, costs
  and troubleshooting.
- [User guide](user-guide.md): for workflow authors and lab users. The `biffix.yaml` manifest,
  registering workflows, launching and monitoring runs, resuming, experiments, data and costs.
- [Developing](developing.md): working on biffix itself. Layout, toolchains, test suites,
  generated code, and the checklist for changing a contract.
- [Run provenance](provenance.md): the Workflow Run RO-Crate biffix produces for every run, and
  the biffix-specific terms it uses.

Elsewhere in the repository:

- [infra/README.md](../infra/README.md): the full operational reference for the CDK app and the
  deploy scripts.
- [infra/biffix-deploy.example.yaml](../infra/biffix-deploy.example.yaml): every deploy config
  key, with its default.
- [dev/README.md](../dev/README.md): the local Postgres and running the API server locally.
- [plugins/snakemake-logger-plugin-biffix](../plugins/snakemake-logger-plugin-biffix/README.md)
  and [plugins/nf-biffix](../plugins/nf-biffix/README.md): the engine plugins.
- [schemas/](../schemas/): the generated JSON Schemas for the event, wire and form contracts.
