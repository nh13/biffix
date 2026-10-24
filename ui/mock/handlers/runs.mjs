// Run routes (runs.rs, tasks.rs, submit.rs): the list, a run's detail, tasks, logs, attempts,
// samples, reports, provenance and failure summary; launching, cancelling and renaming a run. And
// the run views every resource shares: a run's detail and summary as the server builds them.
import { ATTEMPT_VIEWS, LAB_FAILURES, LAB_REPORTS } from '../fixtures/lab.mjs';
import { OUTPUTS } from '../fixtures/outputs.mjs';
import { REFERENCES } from '../fixtures/references.mjs';
import {
  FAILED_BEFORE_LAUNCH,
  PAGING_RUNS,
  PAGING_WORKFLOW,
  RUNS,
  withTaskCounts,
} from '../fixtures/runs.mjs';
import { SETTINGS } from '../fixtures/settings.mjs';
import { outputPrefixOf, perRunKey, projectBucket } from '../lib/buckets.mjs';
import { now, wallClock } from '../lib/clock.mjs';
import { globMatches } from '../lib/glob.mjs';
import { jsonToParams, missingField, readJson, readJsonOrNull } from '../lib/http.mjs';
import { shallowestMultiqc } from '../lib/multiqc.mjs';
import { badLimit, bytewise, clampInt } from '../lib/paging.mjs';
import { boolParam, intParam } from '../lib/query.mjs';
import { ACTIVE, TERMINAL } from '../lib/run-states.mjs';
import { sampleCounts, sampleGrid } from '../lib/sample-grid.mjs';
import { CANCEL_REQUESTED, ME, ME_LABEL, NOTIFICATIONS_READ } from '../state.mjs';
import { membershipProblem } from './experiments.mjs';
import { dismissRuns, needsAttention, readRun } from './me.mjs';
import { frozenCatalog, versionById, workflowTitleOf } from './workflows.mjs';

// End `r` cancelled at `by`'s request (a person's label): only now does it say who cancelled it, as
// the backend's fold records it only on a run that ended cancelled. A task it left open reads
// cancelled, as the fold's `effective_task_state` has it.
const endCancelled = (r, by) => {
  CANCEL_REQUESTED.delete(r.run_id);
  r.state = 'cancelled';
  r.completed_at = wallClock();
  if (by) r.cancelled_by = by;
  for (const t of r.tasks ?? []) {
    if (t.state === 'running' || t.state === 'queued') t.state = 'cancelled';
  }
  withTaskCounts(r);
};

// The lifecycle and resume facts the server computes for a run
// (crates/biffix-backend/src/api/runs.rs, and the submit path's own `resumable` check in
// api/submit.rs): `terminal` on every summary; on a detail, the effective output prefix, whether
// `--resume-from` would be accepted, the Nextflow resume cache, the prefix kind and the DAG.
const withLifecycle = (r) => ({ ...r, terminal: TERMINAL.has(r.state) });
// A task as the backend serves it: one that ran on Batch (attempted, not a cache hit) names its
// display attempt's job, numbered as the attempts endpoint numbers them (job-1, job-2, …), unless the
// fixture names one. A cache hit or a never-attempted task has none.
function withDisplayJob(t) {
  if (t.batch_job_id != null || !(t.attempts > 0) || t.cached) return t;
  return { ...t, batch_job_id: `job-${t.attempts}` };
}

// `RunCost::priced_usd`: a run's cost as one number (its estimate) once a job is priced, else none.
export const pricedUsd = (r) => (r.cost?.priced_jobs > 0 ? r.cost.estimated_usd : undefined);
// `RunSummary.cost_pending`: the run has jobs not priced yet (its cost is not final).
const costPending = (r) => (r.cost?.pending_jobs ?? 0) > 0;

// `RunDetail.launched_at_micros`: when the run launched (its anchor), which a run still preparing
// its submit-time images has not; else when it was submitted (no image wait), unless a run says
// otherwise (`launched_at`, epoch seconds).
function launchedAtMicros(r) {
  if (r.state === 'preparing_images') return undefined;
  const at = r.launched_at ?? r.submitted_at;
  return at != null ? Math.round(at * 1e6) : undefined;
}

// A run with `liveClock`: its times move with the wall clock, so it stays "started 10 min ago"
// however long the mock has been up (an e2e run reads its time left against a typical run).
let liveClockAt = now;
export function keepLiveRunsCurrent() {
  const t = wallClock();
  const delta = t - liveClockAt;
  if (delta <= 0) return;
  liveClockAt = t;
  const shift = (o, k) => {
    if (typeof o[k] === 'number') o[k] += delta;
  };
  for (const r of RUNS) {
    if (!r.liveClock) continue;
    shift(r, 'submitted_at');
    shift(r, 'launched_at');
    for (const task of r.tasks ?? []) {
      shift(task, 'started_at');
      shift(task, 'finished_at');
    }
  }
}

/** Whether a run can be resumed: it ended, its work is kept, and a Nextflow run has a session. */
const isResumable = (r) =>
  TERMINAL.has(r.state) &&
  (r.engine === 'snakemake' || r.engine === 'nextflow') &&
  !r.work_deleted_at_micros &&
  (r.engine !== 'nextflow' || !!r.engine_session_id);

function runDetail(r) {
  const { dag_nodes, dag_edges, liveClock: _clock, launched_at: _launched, ...rest } = r;
  const usd = pricedUsd(r);
  const prefix = outputPrefixOf(r);
  const key = prefix.replace(/^s3:\/\/[^/]+\//, '');
  const nextflow = r.engine === 'nextflow';
  const resumable = isResumable(r);
  // When its version was registered (`get_run`'s registry read; absent for one it no longer has).
  const registered = r.workflow ? versionById(r.workflow)?.created_at_micros : undefined;
  const title = workflowTitleOf(r.workflow);
  const launched = launchedAtMicros(r);
  return {
    ...withLifecycle(rest),
    ...(title ? { workflow_title: title } : {}),
    ...(launched != null ? { launched_at_micros: launched } : {}),
    ...(usd != null ? { cost_usd: usd } : {}),
    cost_pending: costPending(r),
    ...(r.resumed_from ? { resumed: true } : {}),
    ...(registered != null ? { workflow_registered_at_micros: registered } : {}),
    ...(rest.tasks ? { tasks: rest.tasks.map(withDisplayJob) } : {}),
    output_prefix: prefix,
    resumable,
    prefix_kind:
      key === perRunKey(r.run_id)
        ? 'isolated'
        : r.resumed_from && key === perRunKey(r.resumed_from)
          ? 'resumed'
          : 'custom',
    ...(nextflow && r.engine_session_id
      ? {
          resume_cache_uri: `${prefix.replace(/\/?$/, '/')}.nextflow/cache/${r.engine_session_id}/`,
        }
      : {}),
    dag: buildDag(dag_nodes ?? [], dag_edges ?? [], r.tasks ?? []),
  };
}

// The backend's `build_dag`: the declared nodes plus every task group, sorted by id; each task
// counts on its group's node by state (queued as pending; cached also as cached); edges only
// between known nodes. So a run's graph always agrees with its task list.
function buildDag(ids, edges, tasks) {
  const node = (id) => ({ id, pending: 0, running: 0, succeeded: 0, failed: 0, cached: 0 });
  const nodes = new Map(ids.map((id) => [id, node(id)]));
  for (const t of tasks) {
    if (!t.group) continue;
    if (!nodes.has(t.group)) nodes.set(t.group, node(t.group));
    const n = nodes.get(t.group);
    n[['succeeded', 'running', 'failed'].includes(t.state) ? t.state : 'pending'] += 1;
    if (t.cached) n.cached += 1;
  }
  return {
    nodes: [...nodes.values()].sort((a, b) => bytewise(a.id, b.id)),
    edges: edges.filter(([from, to]) => nodes.has(from) && nodes.has(to)),
  };
}
/** api/runs.rs `RUN_RENAME_BODY_ERROR`. */
const RUN_RENAME_BODY_ERROR =
  'the body must be JSON naming `run_name`: a name, or null to clear it';

// The backend's `parse_filter` (biffix-api `RunFilter`): absent or `all` is every run; any other
// spelling, a blank one too, is a 400, not an empty list. `null` for a filter it refuses.
const RUN_FILTERS = 'all, active, submitted, completed, succeeded or failed';
function filtered(filter, runs = RUNS) {
  if (filter == null || filter === 'all') return runs;
  if (filter === 'active') return runs.filter((r) => ACTIVE.has(r.state));
  if (filter === 'completed') return runs.filter((r) => TERMINAL.has(r.state));
  if (['submitted', 'succeeded', 'failed'].includes(filter))
    return runs.filter((r) => r.state === filter);
  return null;
}

// The backend's run-list keyset (pg_store `list_runs`): newest submitted first (unsubmitted runs
// first of all), then run id descending (bytewise). A cursor is the sort key of a page's edge row.
const runSortKey = (r) => [r.submitted_at ?? Number.MAX_SAFE_INTEGER, r.run_id];
const runCursor = (r) => runSortKey(r).join('|');
export function runOrder(a, b) {
  const [as, ai] = runSortKey(a);
  const [bs, bi] = runSortKey(b);
  return bs - as || bytewise(bi, ai);
}
/** A cursor's edge, or `null` for one the mock did not issue (`decode_cursor`: no `|`, a key that
 * is not an integer, a run id holding a NUL), which reads as no cursor. */
function cursorKey(c) {
  const m = /^(-?\d+)\|([^\0]*)$/.exec(c);
  return m ? { submitted_at: Number(m[1]), run_id: m[2] } : null;
}

// `RunQuery::page_size`: the asked limit (default 50) clamped to 1..=MAX_RUN_PAGE (200).
const MAX_RUN_PAGE = 200;

/** `GET /runs` (and `POST /runs/query`): filter, search, scope, then one keyset page; `{ error }` (a
 * 400, worded as runs.rs words it) for a request the backend refuses. */
function runList(params) {
  // Trimmed, blank as none, as the backend reads it.
  const workflow = (params.get('workflow') ?? '').trim();
  const experiment = (params.get('experiment') ?? '').trim();
  // `attention` implies `mine`.
  const inBody = params.get(BODY_FLAG) === '1';
  const asked = [boolParam(params, 'attention', inBody), boolParam(params, 'mine', inBody)];
  const bad = asked.find((b) => b.error);
  if (bad) return { status: bad.error[0], error: bad.error[1] };
  const attention = asked[0].value;
  const mine = asked[1].value || attention;
  const q = (params.get('q') ?? '').trim().toLowerCase();
  // `ids` (POST /runs/query only: its JSON list, carried here as JSON with the body flag; a GET's
  // query string carries no list, so `GET /runs?ids=…` is a 400, as serde refuses it): only those
  // runs, blanks ignored; a list of blanks only names no run (never every run); more than a page's
  // worth is a 400, as runs.rs refuses.
  const idsRaw = params.get('ids');
  const notAList = { error: '`ids` must be a list of run ids' };
  if (idsRaw != null && !inBody) return notAList;
  let given = [];
  try {
    given = idsRaw ? JSON.parse(idsRaw) : [];
  } catch {
    return notAList;
  }
  if (!Array.isArray(given)) return notAList;
  const ids = given.map((id) => String(id).trim()).filter(Boolean);
  if (ids.length > MAX_RUN_PAGE)
    return { error: `\`ids\` names ${ids.length} runs; at most ${MAX_RUN_PAGE} per request` };
  const source = workflow === PAGING_WORKFLOW ? PAGING_RUNS : RUNS;
  const filter = params.get('filter');
  const byState = filtered(filter, source);
  if (!byState) return { error: `unknown filter \`${filter}\` (${RUN_FILTERS})` };
  const all = byState
    .filter((r) => !given.length || ids.includes(r.run_id))
    .filter((r) => !workflow || r.workflow === workflow)
    .filter((r) => !experiment || r.experiment_id === experiment)
    .filter((r) => !mine || r.submitter === ME)
    .filter((r) => !attention || needsAttention(r))
    .filter(
      (r) =>
        !q ||
        [r.run_id, r.run_name, r.workflow_name, workflowTitleOf(r.workflow), r.workflow]
          .concat([r.submitter, r.submitter_label, r.project, r.engine])
          .some((f) => (f ?? '').toLowerCase().includes(q)),
    )
    .sort(runOrder);
  const limit = clampInt(params.get('limit'), 50, 1, MAX_RUN_PAGE);
  if (limit == null) {
    const [status, error] = badLimit(params.get('limit'), inBody);
    return { status, error };
  }
  // As pg_store `list_runs`: `after` wins when both are given, and a token that does not decode is
  // no cursor (the first page).
  const after = params.get('after');
  const before = params.get('before');
  const edge = after ? cursorKey(after) : before ? cursorKey(before) : null;
  let page;
  if (!edge) page = all.slice(0, limit);
  else if (after) page = all.filter((r) => runOrder(r, edge) > 0).slice(0, limit);
  else page = all.filter((r) => runOrder(r, edge) < 0).slice(-limit);
  return {
    runs: page.map(summary),
    total: all.length,
    limit,
    first_cursor: page.length ? runCursor(page[0]) : null,
    last_cursor: page.length ? runCursor(page[page.length - 1]) : null,
  };
}

// The Jobs view's parameters, as `tasks.rs` `parse_param` accepts them: absent is the default, an
// unknown value a 400 worded as the backend words it.
const TASK_PARAMS = {
  state: [['all', 'active', 'succeeded', 'failed'], 'all, active, succeeded or failed'],
  sort: [['key', 'state', 'duration', 'started'], 'key, state, duration or started'],
  order: [['asc', 'desc'], 'asc or desc'],
};
function taskParamError(params) {
  for (const [name, [accepted, words]] of Object.entries(TASK_PARAMS)) {
    const v = params.get(name);
    if (v != null && !accepted.includes(v)) return `unknown ${name} \`${v}\` (${words})`;
  }
  return null;
}

// The backend's `page_tasks` (task_page.rs): state/group/q/task_key filters, a sort (ties by key,
// bytewise), `order=desc` reversing it, offset paging (limit 1..=1000, default 200), chip counts and
// each task's dependents (`feeds`: sorted, unique, never itself).
function pageTasks(all, params, offset, limit) {
  const state = params.get('state') ?? 'all';
  const group = params.get('group');
  const q = (params.get('q') ?? '').trim().toLowerCase();
  const taskKey = params.get('task_key');
  const sort = params.get('sort') ?? 'key';
  const matches = (f, t) =>
    f === 'all' || (f === 'active' ? t.state === 'running' || t.state === 'queued' : t.state === f);
  const narrowed = all.filter(
    (t) =>
      (!group || t.group === group) &&
      (!taskKey || t.task_key === taskKey) &&
      (!q || t.task_key.toLowerCase().includes(q)),
  );
  const rank = { failed: 0, running: 1, queued: 2, cancelled: 3, succeeded: 4 };
  const byKey = (a, b) => bytewise(a.task_key, b.task_key);
  // Longest (or latest) first; a task that never started after every one that did.
  const startedFirst = (va, vb) => (va == null) - (vb == null) || (vb ?? 0) - (va ?? 0);
  // A running task runs until the request, by the clock a live run's times move with.
  const at = wallClock();
  const duration = (t) => (t.started_at == null ? null : (t.finished_at ?? at) - t.started_at);
  const order = {
    key: byKey,
    state: (a, b) => (rank[a.state] ?? 5) - (rank[b.state] ?? 5) || byKey(a, b),
    duration: (a, b) => startedFirst(duration(a), duration(b)) || byKey(a, b),
    started: (a, b) => startedFirst(a.started_at, b.started_at) || byKey(a, b),
  }[sort];
  const rows = narrowed.filter((t) => matches(state, t)).sort(order);
  if (params.get('order') === 'desc') rows.reverse();
  const feeds = {};
  for (const t of all)
    for (const d of t.deps ?? []) if (d !== t.task_key) (feeds[d] ??= new Set()).add(t.task_key);
  return {
    tasks: rows
      .slice(offset, offset + limit)
      .map((t) => ({ ...withDisplayJob(t), feeds: [...(feeds[t.task_key] ?? [])].sort(bytewise) })),
    total: rows.length,
    offset,
    limit,
    counts: {
      all: narrowed.length,
      active: narrowed.filter((t) => matches('active', t)).length,
      succeeded: narrowed.filter((t) => matches('succeeded', t)).length,
      failed: narrowed.filter((t) => matches('failed', t)).length,
    },
  };
}

// RunPage.runs entries are RunSummary: an allowlist of its fields (RunDetail = RunSummary + the
// detail-only fields), so a field a fixture or a launch adds never leaks into the list.
const SUMMARY_FIELDS = new Set([
  'run_id',
  'run_name',
  'state',
  'engine',
  'project',
  'workflow',
  'workflow_name',
  'submitter',
  'submitter_label',
  'submitted_at',
  'completed_at',
  'completed',
  'total',
  'tasks_observed',
  'tasks_succeeded',
  'tasks_running',
  'tasks_queued',
  'tasks_failed',
  'tasks_cancelled',
  'tasks_cached',
  'outputs_total',
  'outputs_reused',
  'input_uris',
  'experiment_id',
]);
export const summary = (r) => {
  const out = withLifecycle(
    Object.fromEntries(Object.entries(r).filter(([k]) => SUMMARY_FIELDS.has(k))),
  );
  const counts = sampleCounts(sampleGrid(r));
  if (counts) out.samples = counts;
  // The pipeline's friendly title, joined from its version's form (`RunSummary::workflow_title`).
  const title = workflowTitleOf(r.workflow);
  if (title) out.workflow_title = title;
  // A resume says so (`RunSummary::resumed`, omitted when false).
  if (r.resumed_from) out.resumed = true;
  // The run's cost, as the run page shows it (`api::runs::with_list_facts`).
  const usd = pricedUsd(r);
  if (usd != null) out.cost_usd = usd;
  out.cost_pending = costPending(r);
  return out;
};
// What a launch's chosen reference bundle `b` fills through the version's form's `fill`, like the
// server: the params, and the bundle's use the run records.
function bundleParams(b, fill) {
  if (!b) return {};
  return Object.fromEntries(
    Object.entries(fill)
      .filter(([, key]) => key in b.uris)
      .map(([param, key]) => [param, b.uris[key]]),
  );
}
function bundleUse(b, fill) {
  if (!b) return null;
  const keys = Object.values(fill).filter((k) => k in b.uris);
  return { id: b.id, uris: Object.fromEntries(keys.map((k) => [k, b.uris[k]])) };
}

// submit/request.rs: the most retries a submit may ask for, and the longest run name.
const MAX_SUBMIT_RETRIES = 10;
const MAX_RUN_NAME_CHARS = 200;
const RETENTIONS = ['default', 'delete_on_success', 'keep'];
const ENGINE_LABEL = { snakemake: 'Snakemake', nextflow: 'Nextflow' };

/** submit/resume.rs `resume_target`: the 400's message for a resume of `prior` it refuses. */
function resumeProblem(prior, version, requestedPrefix) {
  const id = prior.run_id;
  if (prior.engine !== version.engine)
    return prior.engine
      ? `run ${id} is a ${ENGINE_LABEL[prior.engine] ?? prior.engine} run, not a ${ENGINE_LABEL[version.engine]} run`
      : `run ${id} records no engine, so it is not a ${ENGINE_LABEL[version.engine]} run`;
  if (prior.workflow_name !== version.name || prior.project !== version.project)
    return `run ${id} is a run of ${prior.project ?? '?'}/${prior.workflow_name ?? '?'}, not ${version.project}/${version.name}`;
  if (!TERMINAL.has(prior.state))
    return `run ${id} is still ${prior.state}; only a finished run can be resumed`;
  if (prior.work_deleted_at_micros)
    return `run ${id}'s intermediate files were deleted, so it can no longer be resumed; launch it again instead`;
  if (prior.engine === 'nextflow' && !prior.engine_session_id)
    return `run ${id} has no recorded Nextflow session (it predates resume support)`;
  const prefix = outputPrefixOf(prior);
  const asked = requestedPrefix?.trim();
  if (asked && asked.replace(/\/+$/, '') !== prefix.replace(/\/+$/, ''))
    return `output_prefix must equal the resumed run's (${prefix})`;
  return null;
}

/**
 * submit/request.rs `submit_run`'s refusals, in its order: the request's own checks, the version's
 * readiness, the runs it carries on from, then the reference bundle. `{ status, message }` for the
 * first refusal, else what the checks settled.
 */
function checkLaunch(request, version) {
  const bad = (message) => ({ status: 400, message });
  const retention = request.retention ?? 'default';
  if (!RETENTIONS.includes(retention))
    return bad(`unknown retention \`${retention}\` (default, delete_on_success or keep)`);
  if (version.engine !== 'nextflow' && retention !== 'default')
    return bad('retention applies only to Nextflow runs');
  if (request.retries > MAX_SUBMIT_RETRIES)
    return bad(`retries must be at most ${MAX_SUBMIT_RETRIES}`);
  const runName = (request.run_name ?? '').trim() || null;
  if (runName && [...runName].length > MAX_RUN_NAME_CHARS)
    return bad(`run_name must be at most ${MAX_RUN_NAME_CHARS} characters`);
  // Trimmed, blank as none, as submit records it.
  const experiment = (request.experiment ?? '').trim() || null;
  if (experiment) {
    const problem = membershipProblem(experiment, version.project, false);
    if (problem) return { status: problem[0], message: `\`experiment\`: ${problem[1]}` };
  }
  // Params are an object by field name; `null` is none given.
  const params = request.params ?? {};
  if (typeof params !== 'object' || Array.isArray(params))
    return bad('params must be a JSON object');
  if (!version.ready)
    return { status: 409, message: `this version cannot run yet: ${version.not_ready.join('; ')}` };
  let prior = null;
  if (request.resume_from != null) {
    prior = RUNS.find((r) => r.run_id === request.resume_from);
    if (!prior)
      return { status: 404, message: `\`resume_from\`: unknown run ${request.resume_from}` };
    const problem = resumeProblem(prior, version, request.output_prefix);
    if (problem) return bad(problem);
  }
  const retryOf = (request.retry_of ?? '').trim() || null;
  if (retryOf && !RUNS.some((r) => r.run_id === retryOf))
    return { status: 404, message: `\`retry_of\`: unknown run ${retryOf}` };
  // A chosen reference bundle fills the form's reference params.
  const refs = version.form_schema?.references;
  let bundle = null;
  if (request.reference) {
    if (!refs) return bad('this workflow version takes no reference bundle');
    bundle = REFERENCES.find((b) => b.id === request.reference);
    if (!bundle) return bad(`unknown reference bundle ${request.reference}`);
  }
  return { runName, experiment, params, prior, retryOf, bundle, fill: refs?.fill ?? {} };
}
// Where a launch's input tables are written: under the run's per-run prefix in its project bucket.
const tableUri = (project, runId, p) =>
  `s3://${projectBucket(project)}/${perRunKey(runId)}inputs/${p}.csv`;
function tableParams(project, runId, tables) {
  return Object.fromEntries(Object.keys(tables ?? {}).map((p) => [p, tableUri(project, runId, p)]));
}
function inputTables(project, runId, tables) {
  if (!tables || !Object.keys(tables).length) return null;
  return Object.fromEntries(
    Object.entries(tables).map(([p, rows]) => [p, { uri: tableUri(project, runId, p), rows }]),
  );
}

/**
 * `POST /runs`: a new run of the version, recorded like the rest so the UI can open it. Its facts
 * come from the request and the version, never another run's: no progress, tasks or cost yet; the
 * output prefix is the request's, the resumed run's (a resume reuses its work dir and Nextflow
 * session), or the per-run default.
 */
function launch(request, send) {
  const version = versionById(request.workflow_version_id);
  if (!version) return send(404, `unknown workflow version ${request.workflow_version_id}`);
  const checked = checkLaunch(request, version);
  if (checked.status) return send(checked.status, checked.message);
  const { runName, experiment, params, prior, retryOf, bundle, fill } = checked;
  const run_id = `run_mock${String(RUNS.length).padStart(4, '0')}`;
  // Submit-time images wait in `preparing_images` (the mock never finishes preparing them).
  // Each spelling once, as given; one without a digest is "pinned" to a fake one.
  const images = [...new Set((request.images ?? []).map((i) => i.trim()).filter(Boolean))];
  const submitImages = images.map((ref) => ({
    declared_ref: ref,
    source_ref: ref.includes('@sha256:') ? ref : `${ref}@sha256:m0ck`,
  }));
  const preparing = images.length > 0;
  const state = preparing ? 'preparing_images' : 'submitted';
  const { submit_images: _prior, ...versionProvenance } = version.provenance ?? {};
  const project = version.project;
  const requested = request.output_prefix?.trim();
  const run = withTaskCounts({
    run_id,
    run_name: runName,
    state,
    engine: version.engine,
    project,
    workflow: version.id,
    workflow_name: version.name,
    submitter: ME,
    submitter_label: ME_LABEL,
    submitted_at: wallClock(),
    completed_at: null,
    completed: null,
    total: null,
    outputs_total: 0,
    outputs_reused: 0,
    coordinator_job_id: preparing ? null : `coord-${run_id}`,
    output_prefix: prior
      ? outputPrefixOf(prior)
      : requested
        ? requested.replace(/\/?$/, '/')
        : undefined,
    engine_session_id: prior?.engine_session_id,
    resumed_from: prior?.run_id,
    retention: request.retention,
    experiment_id: experiment ?? undefined,
    // Provenance comes from the launch record: none until the run launches (the mock launches
    // only a run without submit-time images).
    provenance: preparing
      ? undefined
      : { git_sha: version.git_sha, image_digest: version.image_digest, ...versionProvenance },
    submit_images: preparing ? submitImages : undefined,
    preparing: preparing
      ? {
          since_micros: Date.now() * 1000,
          not_ready: images.map((ref) => `image \`${ref}\` is being imported`),
          images: submitImages.map((i) => ({
            name: i.declared_ref,
            declared_ref: i.declared_ref,
            source_ref: i.source_ref,
            kind: 'import',
            state: 'building',
          })),
        }
      : undefined,
    params: {
      ...params,
      ...tableParams(project, run_id, request.tables),
      ...bundleParams(bundle, fill),
    },
    input_tables: inputTables(project, run_id, request.tables),
    reference_bundle: bundleUse(bundle, fill),
    dag_nodes: [],
    dag_edges: [],
    tasks: [],
  });
  RUNS.unshift(run);
  // Trying a run again (a re-run names it in `retry_of`, a resume in `resume_from`) reads it and
  // dismisses it from *Needs attention*.
  for (const tried of [retryOf, prior?.run_id])
    if (tried) {
      readRun(tried);
      dismissRuns([tried]);
    }
  // 202 whether it launched or is preparing its images, as the server answers.
  send(
    202,
    preparing
      ? { run_id, state }
      : { run_id, state, coordinator_job_id: run.coordinator_job_id, assumed_role_arn: 'arn:mock' },
  );
}

/** The run a route names, or `null` once it answered a bare 404. */
function namedRun({ params, send }) {
  const r = RUNS.find((x) => x.run_id === params.run_id);
  if (!r) send(404);
  return r ?? null;
}

/** `POST /runs`: launch. */
function submitRun({ req, send }) {
  readJson(req, send, (request) => launch(request, send));
}

/** `GET /runs` (and `POST /runs/query`). */
function listRuns({ query, send }) {
  const list = runList(query);
  return list.error ? send(list.status ?? 400, list.error) : send(200, list);
}

/** `GET /runs/{run_id}`, `?tasks=false` without its tasks. */
function getRun(c) {
  const r = namedRun(c);
  if (!r) return;
  // A recorded cancel of a running run: its coordinator's FAILED has arrived by now.
  if (CANCEL_REQUESTED.has(r.run_id)) endCancelled(r, CANCEL_REQUESTED.get(r.run_id));
  const d = runDetail(r);
  c.send(200, c.query.get('tasks') === 'false' ? { ...d, tasks: [] } : d);
}

/** `POST /runs/{run_id}/cancel`. Mirrors the backend (cancel.rs `cancel_run`): a finished run is a
 * no-op; a run not yet running ends as cancelled at once; a running one only records the request
 * (and terminates its coordinator, when it has one), and ends when the coordinator's FAILED is
 * folded in (the mock folds it on the run's next read); without a coordinator no FAILED comes. */
function cancelRun(c) {
  const r = RUNS.find((x) => x.run_id === c.params.run_id);
  if (!r) return c.send(404, 'unknown run');
  if (ACTIVE.has(r.state)) {
    // Her own run: its cancelled notification is read ahead, as the backend writes it.
    if (r.submitter === ME) NOTIFICATIONS_READ.add(`${r.run_id}:cancelled`);
    if (r.state !== 'running') endCancelled(r, ME_LABEL);
    else if (r.coordinator_job_id) CANCEL_REQUESTED.set(r.run_id, ME_LABEL);
  }
  c.send(202);
}

/** `POST /runs/{run_id}/name`: rename a run, or clear its name (`null`, or blank). The backend
 * records a `run_renamed` event; here the name is the run's, so it outlives a reload. */
function renameRun({ req, params, send }) {
  readJsonOrNull(req, send, (body) => {
    const field = body && typeof body === 'object' ? body.run_name : undefined;
    if (field !== null && typeof field !== 'string') return send(400, RUN_RENAME_BODY_ERROR);
    const name = field?.trim() || null;
    if (name && [...name].length > 200) return send(400, 'run_name must be at most 200 characters');
    const runId = params.run_id;
    const r = RUNS.find((x) => x.run_id === runId);
    if (!r) return send(404, `unknown run ${runId}`);
    if (name) r.run_name = name;
    else delete r.run_name;
    send(200, summary(r));
  });
}

/** `GET /runs/{run_id}/provenance`: a minimal Workflow Run RO-Crate (the shape, not the full
 * graph). */
function getProvenance(c) {
  if (!namedRun(c)) return;
  const { params, res, send } = c;
  const runId = params.run_id;
  res.setHeader('content-type', 'application/ld+json');
  res.setHeader('content-disposition', `attachment; filename="${runId}-ro-crate-metadata.json"`);
  send(200, {
    '@context': [
      'https://w3id.org/ro/crate/1.3/context',
      'https://w3id.org/ro/terms/workflow-run/context',
    ],
    '@graph': [
      {
        '@id': 'ro-crate-metadata.json',
        '@type': 'CreativeWork',
        about: { '@id': './' },
        conformsTo: { '@id': 'https://w3id.org/ro/crate/1.3' },
      },
      {
        '@id': './',
        '@type': 'Dataset',
        name: `Provenance of biffix run ${runId}`,
        mainEntity: { '@id': '#workflow' },
        mentions: [{ '@id': `#run-${runId}` }],
      },
      { '@id': `#run-${runId}`, '@type': 'CreateAction', identifier: runId },
    ],
  });
}

/** `GET /runs/{run_id}/reports`: a declared coverage plot, then the MultiQC report found by name.
 * The Lab analyses declare theirs (`LAB_REPORTS`): each that exists among the outputs, a per-sample
 * one once per sample. */
function listReports(c) {
  if (!namedRun(c)) return;
  const { params, send } = c;
  const runId = params.run_id;
  const rows = OUTPUTS[runId] ?? [];
  if (LAB_REPORTS[runId])
    return send(200, {
      // Each declaration's index in `reports:` (`declaration`); a detected one has none.
      reports: LAB_REPORTS[runId].flatMap(({ path, samples, ...r }, declaration) =>
        (samples ?? [null]).flatMap((sample) =>
          rows
            .filter((o) => globMatches(sample ? path.replace('{sample}', sample) : path, o.path))
            .sort((a, b) => bytewise(a.s3_uri, b.s3_uri))
            .slice(0, sample ? 4 : 1)
            .map((output) => ({
              ...r,
              ...(sample ? { sample } : {}),
              ...(r.kind === 'detected' ? {} : { declaration }),
              output,
            })),
        ),
      ),
    });
  const coverage = rows.find((o) => o.path === 'qc/coverage.svg');
  const multiqc = shallowestMultiqc(rows);
  send(200, {
    reports: [
      ...(coverage
        ? [
            {
              name: 'Coverage',
              description: 'Depth across the genome',
              kind: 'declared',
              format: 'image',
              declaration: 0,
              output: coverage,
            },
          ]
        : []),
      ...(multiqc
        ? [
            {
              name: 'MultiQC',
              description: "Quality control across the run's samples",
              kind: 'multiqc',
              format: 'html',
              output: multiqc,
            },
          ]
        : []),
    ],
  });
}

/** `GET /runs/{run_id}/samples`: the per-sample progress grid. */
function getRunSamples(c) {
  const run = namedRun(c);
  if (run) c.send(200, sampleGrid(run));
}

/** `GET /runs/{run_id}/failure-summary`. */
function failureSummary(c) {
  const run = namedRun(c);
  if (!run) return;
  const { send } = c;
  if (run.state !== 'failed') return send(200, { failed_steps: 0, items: [] });
  // Who to ask: the run's pipeline's own contact, else the site's.
  const contact = frozenCatalog(run.workflow)?.contact ?? SETTINGS.support;
  const support = contact ? { support: contact } : {};
  // As the server does: a resume it would refuse (the run is not resumable) is a re-run.
  const resumable = isResumable(run);
  const tryAgain = (mode) => (mode === 'resume' && !resumable ? 'rerun' : mode);
  const lab = LAB_FAILURES[run.run_id];
  if (lab)
    return send(200, {
      ...lab,
      items: lab.items.map((i) => ({ ...i, try_again: tryAgain(i.try_again) })),
      ...support,
    });
  const beforeLaunch = FAILED_BEFORE_LAUNCH.get(run.run_id);
  if (beforeLaunch) {
    // The control plane's reason, at the stage it recorded (as `failure_summary` words it).
    const imagePreparation = beforeLaunch.stage === 'image_preparation';
    return send(200, {
      failed_steps: 0,
      items: [
        {
          step: imagePreparation ? 'image preparation' : 'launch',
          cause: 'launch_failed',
          message: beforeLaunch.reason,
          advice: imagePreparation
            ? "Fix the image reference or its registry, or rebuild it from the workflow's page, then submit again."
            : "The run could not be launched; check the deploy and the workflow version's engine runtime, then submit again.",
          attempts: 0,
          log_tail: [],
          reason: beforeLaunch.reason,
          stage: beforeLaunch.stage ?? 'launch',
          try_again: 'rerun',
        },
      ],
      ...support,
    });
  }
  send(200, {
    failed_steps: 1,
    items: [
      {
        task_key: 'align:sample=BAD01',
        sample: 'BAD01',
        step: 'align',
        cause: 'out_of_memory',
        try_again: tryAgain('resume'),
        message: 'it ran out of memory',
        exit_code: 137,
        batch_job_id: 'b2c4e6f8-1a3c-4e5a-9b7d-3f5a7c9e1b2d',
        advice:
          'Give this step more memory (its resources in the workflow), or split its input into smaller pieces.',
        attempts: 3,
        log_tail: [
          '[M::bwa_idx_load_from_disk] read 3171 ALT contigs',
          '[M::process] read 400000 sequences (60000000 bp)...',
          'Killed',
        ],
      },
    ],
    ...support,
  });
}

/** `POST /runs/{run_id}/tasks/query`: one page of the Jobs view. */
export function listTasks(c) {
  const r = namedRun(c);
  if (!r) return;
  const bad = taskParamError(c.query);
  if (bad) return c.send(400, bad);
  const limit = clampInt(c.query.get('limit'), 200, 1, 1000);
  if (limit == null) return c.send(...badLimit(c.query.get('limit'), true));
  const offset = intParam(c.query, 'offset', 0, true);
  if (offset.error) return c.send(...offset.error);
  // A run's tasks log to their project's group (`TaskPage.log_group`).
  const logGroup = r.project ? { log_group: `/biffix/project-${r.project}/batch` } : {};
  const page = pageTasks(r.tasks ?? [], c.query, Math.max(0, offset.value), limit);
  c.send(200, { ...page, ...logGroup });
}

/**
 * One page of a log stream, as logs.rs `log_page` answers it: the LogPage contract (biffix-api
 * `LogPage` / `LogLine`): a stream name, `ts_ms` in epoch milliseconds, and CloudWatch-style
 * cursors. No stream yet is an empty page, not a 404; a `direction` it does not know is a 400. The
 * first page (no cursor) is the whole stream; paging from any cursor returns nothing and hands the
 * same token back, which is how CloudWatch says "caught up" (forward) and "start of stream"
 * (backward).
 */
function logPage(stream, who, query, send) {
  const direction = query.get('direction');
  if (direction != null && direction !== 'forward' && direction !== 'backward')
    return send(400, `unknown direction ${JSON.stringify(direction)}: \`forward\` or \`backward\``);
  const page = (events, forward, backward) =>
    send(200, {
      log_stream: stream,
      events,
      next_forward_token: forward,
      next_backward_token: backward,
    });
  if (!stream) return page([], null, null);
  const cursor = query.get('cursor');
  if (cursor) return page([], cursor, cursor);
  page(
    [1, 2, 3].map((i) => ({
      ts_ms: (wallClock() - (4 - i) * 60) * 1000,
      message: `[mock] ${who}: log line ${i}`,
    })),
    'f/mock-end',
    'b/mock-start',
  );
}

/** `POST /runs/{run_id}/task-logs/query`: one task's log, or one of its attempts'
 * (`job`), as logs.rs `task_log_page` derives the stream: a task the run does not have, or a job
 * that is not one of its attempts, is a 404; a task never run on Batch (queued, a cache hit) has
 * no stream yet. */
function taskLogs(c) {
  const r = namedRun(c);
  if (!r) return;
  const { query, send } = c;
  const key = query.get('task');
  // `TaskLogsRequest.task` is required: a body without it is axum's 422.
  if (key == null) return send(422, missingField('task'));
  const t = (r.tasks ?? []).map(withDisplayJob).find((x) => x.task_key === key);
  if (!t) return send(404);
  const job = query.get('job');
  const jobs = [
    ...Array.from({ length: t.attempts ?? 0 }, (_, i) => `job-${i + 1}`),
    t.batch_job_id,
  ];
  if (job != null && !jobs.includes(job)) return send(404);
  // A queued task's job has no host yet, so no stream.
  const ranOnBatch = t.log_stream != null || (t.batch_job_id != null && t.state !== 'queued');
  logPage(ranOnBatch ? (t.log_stream ?? `mock/${key}`) : null, key, query, send);
}

/** `GET /runs/{run_id}/coordinator-logs`: the run's coordinator's log, which has no stream until
 * the coordinator starts (a run preparing its images, or only submitted). */
function coordinatorLogs(c) {
  const r = namedRun(c);
  if (!r) return;
  const started = r.coordinator_job_id && !['preparing_images', 'submitted'].includes(r.state);
  logPage(started ? `mock/coordinator/${r.run_id}` : null, 'coordinator', c.query, c.send);
}

/** `POST /runs/{run_id}/attempts/query`: a task's tries. */
function taskAttempts({ params, query, send }) {
  const key = query.get('task');
  const r = RUNS.find((x) => x.run_id === params.run_id);
  const t = r?.tasks?.find((x) => x.task_key === key);
  if (!t) return send(404);
  send(200, {
    task_key: key,
    attempts: Array.from({ length: t.attempts }, (_, i) => ({
      batch_job_id: `job-${i + 1}`,
      attempt: i + 1,
      state: i + 1 < t.attempts ? 'failed' : t.state,
      display: i + 1 === t.attempts,
      // The display attempt asked for the task's request; an earlier one for half the memory
      // (an engine may raise memory on retry).
      requested_vcpus: t.requested_vcpus,
      requested_memory_mib:
        t.requested_memory_mib != null && i + 1 < t.attempts
          ? t.requested_memory_mib / 2
          : t.requested_memory_mib,
      ...(ATTEMPT_VIEWS.get(`${r.run_id}\u0000${key}`)?.[i] ?? {}),
    })),
  });
}

// Marks a query rewritten from a POST body (only a body carries a list, such as `/runs/query`'s
// `ids`, which travels as JSON).
const BODY_FLAG = '__from_body';

/**
 * The reads that can name a sample (`/runs/query`, `…/tasks/query`, `…/task-logs/query`,
 * `…/attempts/query`) carry their parameters as a JSON body, so no run name, task key or search
 * text appears in a request URL. The body is read into query parameters for the handler (and
 * `/runs/query` answered as `GET /runs`). (`…/outputs/query` takes its own body shape, `roles` as a
 * list.)
 */
const fromBody = (read) => (c) =>
  readJson(c.req, c.send, (body) => {
    const query = jsonToParams(body);
    if (Array.isArray(body?.ids)) query.set('ids', JSON.stringify(body.ids));
    query.set(BODY_FLAG, '1');
    read({ ...c, query });
  });

export const RUN_ROUTES = [
  ['GET', '/runs', listRuns],
  ['POST', '/runs', submitRun],
  ['POST', '/runs/query', fromBody(listRuns)],
  ['GET', '/runs/{run_id}', getRun],
  ['POST', '/runs/{run_id}/cancel', cancelRun],
  ['POST', '/runs/{run_id}/name', renameRun],
  ['GET', '/runs/{run_id}/provenance', getProvenance],
  ['GET', '/runs/{run_id}/reports', listReports],
  ['GET', '/runs/{run_id}/samples', getRunSamples],
  ['GET', '/runs/{run_id}/failure-summary', failureSummary],
  ['POST', '/runs/{run_id}/tasks/query', fromBody(listTasks)],
  ['POST', '/runs/{run_id}/task-logs/query', fromBody(taskLogs)],
  ['GET', '/runs/{run_id}/coordinator-logs', coordinatorLogs],
  ['POST', '/runs/{run_id}/attempts/query', fromBody(taskAttempts)],
];
