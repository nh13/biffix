// A project's own bucket (the deploy's per-project bucket), and the backend's per-run default
// output prefix in it (`runs::output_prefix::effective_output_prefix`), which a fixture run that
// names no `output_prefix` stands for having recorded.
export const projectBucket = (project) => `${project ?? 'genomics'}-project`;
export const perRunKey = (id) => `biffix/runs/${id}/`;
export const outputPrefixOf = (r) =>
  r.output_prefix ?? `s3://${projectBucket(r.project)}/${perRunKey(r.run_id)}`;
