// Runs: the runs list, one run and its tasks, logs and attempts, and launching, cancelling and
// renaming a run.

import type { components } from '#lib/generated/api.js';
import type { SampleGrid } from '#lib/generated/sample-progress.js';
import { API_PATHS, get, pathFor, request, requestJson, withQuery, type JsonBody } from './client';

export type RunPage = JsonBody<'list_runs'>;
export type RunSummary = RunPage['runs'][number];
export type RunDetail = JsonBody<'get_run'>;
export type Task = RunDetail['tasks'][number];

/** Which runs by state: the server's filter set (biffix-api's `RunFilter`). */
export type RunFilter = components['schemas']['RunFilter'];

export interface ListRunsOptions {
  q?: string;
  /** Exact frozen workflow-version id to scope to (the runs of one registered version). */
  workflow?: string;
  /** Only the caller's own runs. */
  mine?: boolean;
  /** Only the caller's failed runs not dismissed (*Needs attention*; implies mine). */
  attention?: boolean;
  limit?: number;
  /** Only the runs in this experiment. */
  experiment?: string;
  /** Only these runs, by id (at most 200): to name runs known only by id. */
  ids?: string[];
  /** Keyset cursor: fetch the next (older) page, from a prior page's `last_cursor`. */
  after?: string;
  /** Keyset cursor: fetch the previous (newer) page, from a prior page's `first_cursor`. */
  before?: string;
}

/**
 * One page of runs. The filters go in a POST body, not a query string: the search text can be a run
 * name, which can name a sample, and must not reach a request URL.
 */
export function listRuns(filter: RunFilter, opts: ListRunsOptions = {}): Promise<RunPage> {
  const body: components['schemas']['RunQueryRequest'] = {};
  if (filter !== 'all') body.filter = filter;
  if (opts.q) body.q = opts.q;
  if (opts.workflow) body.workflow = opts.workflow;
  if (opts.mine) body.mine = true;
  if (opts.attention) body.attention = true;
  if (opts.experiment) body.experiment = opts.experiment;
  if (opts.ids && opts.ids.length > 0) body.ids = opts.ids;
  if (opts.limit != null) body.limit = opts.limit;
  if (opts.after) body.after = opts.after;
  if (opts.before) body.before = opts.before;
  return requestJson<RunPage>(API_PATHS.runsQuery, { method: 'POST', body });
}

export function getRun(
  id: string,
  opts: {
    /** false: leave out the task table (page it with `listTasks` instead). */
    tasks?: boolean;
  } = {},
): Promise<RunDetail> {
  const params = new URLSearchParams();
  if (opts.tasks === false) params.set('tasks', 'false');
  return get<RunDetail>(withQuery(pathFor(API_PATHS.run, { run_id: id }), params));
}

export type TaskPage = JsonBody<'query_tasks'>;
/** Which tasks by state: the Jobs view's chips (biffix-api's `TaskStateFilter`). */
export type TaskStateFilter = components['schemas']['TaskStateFilter'];
export type TaskSort = 'key' | 'state' | 'duration' | 'started';

export type FailureSummary = JsonBody<'failure_summary'>;

/** Why a failed run failed: its first failed steps' causes, advice and log tails. */
export function getFailureSummary(runId: string): Promise<FailureSummary> {
  return get<FailureSummary>(pathFor(API_PATHS.failureSummary, { run_id: runId }));
}

export type { SampleGrid };

/** A run's samples × steps (per-sample progress). */
export function getRunSamples(runId: string): Promise<SampleGrid> {
  return get<SampleGrid>(pathFor(API_PATHS.runSamples, { run_id: runId }));
}

export type RunReportList = JsonBody<'list_reports'>;
export type RunReport = RunReportList['reports'][number];

/** The run's reports: the ones its workflow declared, then a MultiQC report. */
export function listRunReports(runId: string): Promise<RunReportList> {
  return get<RunReportList>(pathFor(API_PATHS.reports, { run_id: runId }));
}

/**
 * One page of a run's tasks, filtered and sorted server-side. The filters go in a POST body, not a
 * query string: the search text and a task key can name a sample, and must not reach a request URL.
 */
export function listTasks(
  runId: string,
  opts: {
    state?: TaskStateFilter;
    /** Only this group (rule/process). */
    group?: string | null;
    /** Substring of the task key. */
    q?: string | null;
    /** Only the task with exactly this key. */
    taskKey?: string | null;
    sort?: TaskSort;
    descending?: boolean;
    offset?: number;
    limit?: number;
  } = {},
): Promise<TaskPage> {
  const body: components['schemas']['TaskQueryRequest'] = {};
  if (opts.state && opts.state !== 'all') body.state = opts.state;
  if (opts.group) body.group = opts.group;
  if (opts.q) body.q = opts.q;
  if (opts.taskKey) body.task_key = opts.taskKey;
  if (opts.sort && opts.sort !== 'key') body.sort = opts.sort;
  if (opts.descending) body.order = 'desc';
  if (opts.offset) body.offset = opts.offset;
  if (opts.limit != null) body.limit = opts.limit;
  return requestJson<TaskPage>(pathFor(API_PATHS.tasksQuery, { run_id: runId }), {
    method: 'POST',
    body,
  });
}

export type LogPage = JsonBody<'query_task_logs'>;

/**
 * Fetch a page of a task's CloudWatch log stream, served through biffix (ViewRuns-gated). The task
 * key goes in a POST body: it names a sample, and must not reach a request URL.
 */
export function getTaskLogs(
  runId: string,
  taskKey: string,
  opts: {
    cursor?: string;
    direction?: 'forward' | 'backward';
    limit?: number;
    /** One attempt's Batch job id; omitted ⇒ the task's display attempt. */
    job?: string;
  } = {},
): Promise<LogPage> {
  const body: components['schemas']['TaskLogsRequest'] = { task: taskKey };
  if (opts.job) body.job = opts.job;
  if (opts.cursor) body.cursor = opts.cursor;
  if (opts.direction) body.direction = opts.direction;
  if (opts.limit != null) body.limit = opts.limit;
  return requestJson<LogPage>(pathFor(API_PATHS.taskLogsQuery, { run_id: runId }), {
    method: 'POST',
    body,
  });
}

/** A page of the run's coordinator (workflow driver) log. */
export function getCoordinatorLogs(
  runId: string,
  opts: { cursor?: string; direction?: 'forward' | 'backward'; limit?: number } = {},
): Promise<LogPage> {
  const params = new URLSearchParams();
  if (opts.cursor) params.set('cursor', opts.cursor);
  if (opts.direction) params.set('direction', opts.direction);
  if (opts.limit != null) params.set('limit', String(opts.limit));
  return get<LogPage>(withQuery(pathFor(API_PATHS.coordinatorLogs, { run_id: runId }), params));
}

export type TaskAttemptList = JsonBody<'query_task_attempts'>;
export type TaskAttempt = TaskAttemptList['attempts'][number];
export type Host = NonNullable<TaskAttempt['host']>;

/**
 * Every AWS Batch attempt of one task (e.g. a spot-reclaimed attempt and its retry). The task key
 * goes in a POST body, not the request URL.
 */
export function getAttempts(runId: string, taskKey: string): Promise<TaskAttemptList> {
  const body: components['schemas']['TaskAttemptsRequest'] = { task: taskKey };
  return requestJson<TaskAttemptList>(pathFor(API_PATHS.attemptsQuery, { run_id: runId }), {
    method: 'POST',
    body,
  });
}

export type RunSubmitRequest = components['schemas']['RunSubmitRequest'];
export type SubmittedRun = components['schemas']['SubmittedRun'];

/** Launch a run (`POST /runs`); the server validates the params against the version's form. */
export function submitRun(body: RunSubmitRequest): Promise<SubmittedRun> {
  return requestJson<SubmittedRun>(API_PATHS.runs, {
    method: 'POST',
    body,
    forbidden: "You don't have permission to launch runs",
  });
}

/**
 * Cancel a run: the backend terminates its coordinator and the run ends as cancelled. Resolves once
 * the request is accepted (the run's state then changes on the next poll). A 403 means the caller
 * lacks permission to cancel runs.
 */
export async function cancelRun(runId: string): Promise<void> {
  await request(pathFor(API_PATHS.cancel, { run_id: runId }), {
    method: 'POST',
    forbidden: "You don't have permission to cancel runs",
    // The run is being launched right now (or the store is briefly unavailable).
    unavailable: 'Cancelling is unavailable; retry in a moment',
  });
}

/** The run's provenance (a Workflow Run RO-Crate's `ro-crate-metadata.json`), built from its log
 * when asked, as a file to save. */
export async function getProvenance(runId: string): Promise<Blob> {
  const path = pathFor(API_PATHS.provenance, { run_id: runId });
  return (await request(path, { accept: 'application/ld+json' })).blob();
}

/**
 * Rename a run, or (`null`, or blank) clear its name so a name derived from its pipeline, day and
 * samples shows. Recorded as an event; answers with the run's summary after.
 */
export function renameRun(runId: string, name: string | null): Promise<RunSummary> {
  const body: components['schemas']['RunRenameRequest'] = { run_name: name };
  return requestJson<RunSummary>(pathFor(API_PATHS.runName, { run_id: runId }), {
    method: 'POST',
    body,
    forbidden: 'You need permission to start analyses to rename one.',
  });
}
