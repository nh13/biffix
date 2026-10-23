// The CLI command that resumes a run. Whether a run can be resumed, its prefix kind and its resume
// cache all come from the API (`RunDetail.resumable`, `prefix_kind`, `resume_cache_uri`), computed
// next to the backend's own resume rules.

/** The run-detail fields {@link resumeCommand} reads. */
export interface ResumeFacts {
  run_id: string;
  workflow?: string | null;
  resumable?: boolean;
}

/**
 * The CLI command that resumes this run, or null when the server says it cannot be resumed (still
 * running, or its intermediate files or Nextflow session are gone). The CLI inherits the run's
 * params (secrets excepted), so the command needs none; `--param k=v` changes one.
 */
export function resumeCommand(run: ResumeFacts): string | null {
  if (!run.resumable || !run.workflow) return null;
  return `biffix runs submit ${run.workflow} --resume-from ${run.run_id}`;
}
