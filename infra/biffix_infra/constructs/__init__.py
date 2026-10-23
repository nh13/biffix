"""The building blocks the stacks compose (`biffix_infra.stacks`).

- An L2 with biffix's defaults (`RustFunction`, `EncryptedBucket`, `InlineScriptProject`,
  `PermissionsBoundary`, `GithubOidcRole`): a subclass, created under the id its stack gives it,
  so the resource keeps the logical id it had before it was factored out.
- A component of several resources (`Coordinator`, `BackendApi`, `BackendSweep`, `IngestPipeline`,
  `Reconcile`, `BatchQueueClass`, `UiDistribution`, `IsolatedWorkerEgress`). Where a component has
  one main resource it is the component's `Default` child, which CDK leaves out of logical ids, so
  that resource's id does not change with the component around it.
- Grant sets that create no resources (`batch_access.grant_coordinator_batch`,
  `ControlPlaneGrants`) and helpers that create resources in their caller's scope (`Alarms`,
  `customer_key`, `nat_instance_provider`), where a logical id must not move.

A stack owns the wiring between components (which grants a Lambda holds, what an environment
variable points at, which Rust binary a Lambda runs); a component owns what its own resources need.
"""
