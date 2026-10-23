// Experiment routes (experiments.rs): the list, one experiment's detail and samples CSV, making and
// editing one, and putting a run in one.
import { EXPERIMENTS } from '../fixtures/experiments.mjs';
import { RUNS } from '../fixtures/runs.mjs';
import { WORKFLOWS } from '../fixtures/workflows.mjs';
import { wallClock } from '../lib/clock.mjs';
import { contentDisposition, csvField, csvTime, jsonEscapedLen } from '../lib/csv.mjs';
import { readJson, readJsonOrNull } from '../lib/http.mjs';
import { bytewise } from '../lib/paging.mjs';
import { boolParam } from '../lib/query.mjs';
import { sampleGrid } from '../lib/sample-grid.mjs';
import { ME, ME_LABEL, ME_SUB } from '../state.mjs';
import { needsAttention } from './me.mjs';
import { pricedUsd, runOrder, summary } from './runs.mjs';

let experimentSeq = 0;

/** An experiment as `Experiment`: its row plus how many runs it has, when the newest started,
 * whether it is the caller's, and how many of the caller's runs in it need her attention. */
function experimentView(e) {
  const runs = RUNS.filter((r) => r.experiment_id === e.id);
  const starts = runs.map((r) => r.submitted_at).filter((t) => t != null);
  return {
    ...e,
    run_count: runs.length,
    ...(starts.length ? { last_run_at: Math.max(...starts) } : {}),
    needs_attention: runs.filter(needsAttention).length,
    mine: isMine(e),
  };
}
/** `?mine=true` (experiments.rs `is_mine`): the caller made it (`created_by` is her bare id, as the
 * backend records it), or a run of hers is in it (a run's submitter is `user:<id>`). */
const isMine = (e) =>
  e.created_by === ME_SUB || RUNS.some((r) => r.experiment_id === e.id && r.submitter === ME);
// The list's order: newest activity first, an experiment's activity being the later of its newest
// run's start and its making (one made today for older runs is new).
const activityOf = (v) => Math.max(v.last_run_at ?? 0, v.created_at);

/** experiments.rs `normalize_name`: the 400 for a blank or too long name (in characters, after
 * trimming), else null. */
function nameRefusal(name) {
  const trimmed = typeof name === 'string' ? name.trim() : '';
  if (!trimmed) return 'an experiment name must not be blank';
  const chars = [...trimmed].length;
  return chars > 200 ? `an experiment name is ${chars} characters; the limit is 200` : null;
}
/** experiments.rs `normalize_description`: the 400 for one over 4000 characters, else null. */
function descriptionRefusal(description) {
  const chars = [...(typeof description === 'string' ? description.trim() : '')].length;
  return chars > 4000
    ? `an experiment description is ${chars} characters; the limit is 4000`
    : null;
}
/** api/experiments.rs `RUN_EXPERIMENT_BODY_ERROR`. */
const RUN_EXPERIMENT_BODY_ERROR =
  'the body must name `experiment_id`: an experiment id, or null to take the run out of its experiment';
/** The store's uniqueness rule: `name` (trimmed) clashes, ignoring case, with an experiment of
 * `project` that is not archived (other than `self`); the 409's message, else null. */
function nameClash(name, project, self) {
  const trimmed = name.trim();
  const clash = EXPERIMENTS.find(
    (e) =>
      e.id !== self &&
      e.project === project &&
      e.archived_at == null &&
      e.name.toLowerCase() === trimmed.toLowerCase(),
  );
  return clash ? `project ${project} already has an experiment named \`${trimmed}\`` : null;
}
/** api/experiments.rs `create_experiment`'s project check (the registry's `has_project`): some
 * registered version lives in it. */
const knownProject = (project) => WORKFLOWS.some((w) => w.project === project);

/** How many runs an experiment's detail holds (the newest; `runs_total` counts them all). */
const EXPERIMENT_DETAIL_RUNS = 200;

/** `GET /experiments/{id}`: its newest runs first (at most `EXPERIMENT_DETAIL_RUNS`), every sample
 * across those (first seen, runs oldest first) with its state in each, and the sum of the priced
 * estimates of all its runs. */
function experimentDetail(e) {
  const all = RUNS.filter((r) => r.experiment_id === e.id).sort(runOrder);
  const runs = all.slice(0, EXPERIMENT_DETAIL_RUNS);
  const samples = new Map();
  for (const r of [...runs].reverse())
    for (const row of sampleGrid(r).samples) {
      if (!samples.has(row.id)) samples.set(row.id, {});
      samples.get(row.id)[r.run_id] = row.state;
    }
  // store.rs `experiment_priced_usd`: a run whose cost has no priced job yet adds nothing.
  const costs = all.map(pricedUsd).filter((usd) => usd != null);
  return {
    experiment: experimentView(e),
    runs: runs.map(summary),
    runs_total: all.length,
    samples: [...samples].map(([id, by_run]) => ({ id, by_run })),
    ...(costs.length ? { total_cost_usd: costs.reduce((a, b) => a + b, 0) } : {}),
  };
}
/** runs/samples_csv.rs `state_words`: `preparing_images` is `Preparing images`. */
const stateWords = (state) => {
  const words = String(state ?? '').replaceAll('_', ' ');
  return words.charAt(0).toUpperCase() + words.slice(1);
};
/** runs/samples_csv.rs `samples_csv_filename` (and the UI's `samplesCsvFilename`). */
function samplesCsvFilename(name) {
  const safe = name.trim().replace(/[/\\:*?"<>|\p{Cc}]/gu, '-');
  return `${safe || 'experiment'}-samples.csv`;
}

/** runs/samples_csv.rs `SamplesCsvLimits::DEPLOYED`: the most runs and (JSON-escaped) bytes one
 * samples download covers; past either the answer is a 413 that says what to do. Each can be
 * lowered for a test (`MOCK_SAMPLES_CSV_MAX_RUNS`, `MOCK_SAMPLES_CSV_MAX_BYTES`). */
const SAMPLES_CSV_MAX_RUNS = Number(process.env.MOCK_SAMPLES_CSV_MAX_RUNS ?? 1000);
const SAMPLES_CSV_MAX_BYTES = Number(process.env.MOCK_SAMPLES_CSV_MAX_BYTES ?? 5 * 1024 * 1024);
/** samples_csv.rs `size_words`: whole MiB when it is a whole number of them, else bytes. */
const sizeWords = (bytes) =>
  bytes % (1024 * 1024) === 0 ? `${bytes / (1024 * 1024)} MiB` : `${bytes} bytes`;
const SAMPLES_CSV_ADVICE =
  'Split the experiment into smaller ones (move some of its runs into another experiment) and download the samples of each.';

/** `secs` (epoch seconds) in time zone `tz`: `{ year, month (1-12), day, hour, minute }`, read
 * with the zone's rules for that instant. */
function civilIn(tz, secs) {
  const parts = new Intl.DateTimeFormat('en-US', {
    timeZone: tz,
    hourCycle: 'h23',
    year: 'numeric',
    month: 'numeric',
    day: 'numeric',
    hour: 'numeric',
    minute: 'numeric',
  }).formatToParts(new Date(secs * 1000));
  const part = (type) => Number(parts.find((p) => p.type === type)?.value);
  return {
    year: part('year'),
    month: part('month'),
    day: part('day'),
    hour: part('hour'),
    minute: part('minute'),
  };
}

/** The analysis's name as the UI shows it and the backend writes it in a samples CSV
 * (`runs/samples_csv.rs` `analysis_name`): its own, else `<pipeline> · <start> · <N samples>`, the
 * start in the downloader's time zone `tz`, with the year when not this year's. */
function csvAnalysisName(r, tz) {
  const s = summary(r);
  const own = s.run_name?.trim();
  if (own) return own;
  const pipeline = s.workflow_title?.trim() || s.workflow_name?.trim() || null;
  let start = null;
  if (s.submitted_at != null) {
    const d = civilIn(tz, s.submitted_at);
    const months = 'Jan Feb Mar Apr May Jun Jul Aug Sep Oct Nov Dec'.split(' ');
    const day = `${months[d.month - 1]} ${d.day}`;
    const year = d.year === civilIn(tz, Date.now() / 1000).year ? '' : `, ${d.year}`;
    const pad = (n) => String(n).padStart(2, '0');
    start = `${day}${year} ${pad(d.hour)}:${pad(d.minute)}`;
  }
  if (!pipeline && !start) return s.run_id;
  const n = s.samples?.total ?? 0;
  const samples = n > 0 ? `${n.toLocaleString('en-US')} ${n === 1 ? 'sample' : 'samples'}` : null;
  return [pipeline, start, samples].filter(Boolean).join(' · ');
}

/** `GET /experiments/{id}/samples.csv` (api/experiments.rs `experiment_samples_csv`, writing it as
 * runs/samples_csv.rs does): every run's samples, runs oldest first, one CRLF row per sample per
 * run, UTF-8 with a byte-order mark; or, over a limit, `{ tooLarge: message }`. */
function experimentSamplesCsv(e, tz = 'UTC') {
  const header = [
    'Sample',
    'Sample status',
    'Reused',
    'Analysis',
    'Run ID',
    'Analysis status',
    'Pipeline',
    'Pipeline version',
    'Engine',
    'Submitted by',
    'Submitted (UTC)',
    'Finished (UTC)',
  ];
  const record = (fields) => fields.map((f) => csvField(String(f ?? ''))).join(',') + '\r\n';
  const runs = RUNS.filter((r) => r.experiment_id === e.id)
    .sort(runOrder)
    .reverse();
  if (runs.length > SAMPLES_CSV_MAX_RUNS)
    return {
      tooLarge: `This experiment has ${runs.length} runs (analyses), more than the ${SAMPLES_CSV_MAX_RUNS} one samples download can cover. ${SAMPLES_CSV_ADVICE}`,
    };
  let body = '﻿' + record(header);
  let size = jsonEscapedLen(body);
  let rows = 0;
  for (const [i, r] of runs.entries()) {
    const analysis = csvAnalysisName(r, tz);
    for (const row of sampleGrid(r).samples) {
      const cells = Object.values(row.cells);
      // Reused only when the sample finished in this run with every one of its steps reused.
      const reused =
        row.state === 'done' && cells.length > 0 && cells.every((c) => c.state === 'reused');
      const line = record([
        row.id,
        stateWords(row.state),
        reused ? 'yes' : 'no',
        analysis,
        r.run_id,
        stateWords(r.state),
        r.workflow_name,
        r.workflow,
        r.engine,
        r.submitter_label ?? r.submitter,
        csvTime(r.submitted_at),
        csvTime(r.completed_at),
      ]);
      body += line;
      size += jsonEscapedLen(line);
      rows += 1;
      if (size > SAMPLES_CSV_MAX_BYTES)
        return {
          tooLarge: `This experiment's samples are too many to download at once: the file would be larger than ${sizeWords(SAMPLES_CSV_MAX_BYTES)}, the most one download can hold (its oldest ${i + 1} of ${runs.length} runs (analyses) already give ${rows} sample rows). ${SAMPLES_CSV_ADVICE}`,
        };
    }
  }
  return { body };
}

/**
 * Why a run cannot go into experiment `id` (a status and message), else null — as the backend's
 * `joinable_experiment`: unknown is a 404 when the experiment is the request's subject
 * (`POST /runs/{id}/experiment`), else a 400 (a submit's `experiment` field); another project's or
 * an archived one is a 400.
 */
export function membershipProblem(id, project, unknownIs404 = true) {
  const e = EXPERIMENTS.find((x) => x.id === id);
  if (!e) return [unknownIs404 ? 404 : 400, `unknown experiment ${id}`];
  if (e.project !== project)
    return [
      400,
      `experiment ${id} is in project ${e.project}, not the run's (${project ?? 'none'})`,
    ];
  if (e.archived_at != null) return [400, `experiment ${id} is archived; restore it to add runs`];
  return null;
}

/** `POST /experiments`: as create_experiment, the name and description, then the project, then the
 * clash. */
function createExperiment({ req, send }) {
  readJson(req, send, (body) => {
    const refusal = nameRefusal(body.name) ?? descriptionRefusal(body.description);
    if (refusal) return send(400, refusal);
    const project = typeof body.project === 'string' ? body.project.trim() : '';
    if (!knownProject(project))
      return send(404, `unknown project ${project}: no registered workflow is in it`);
    const clash = nameClash(body.name, project, null);
    if (clash) return send(409, clash);
    const description = typeof body.description === 'string' ? body.description.trim() : '';
    experimentSeq += 1;
    const e = {
      id: `exp_mock${String(experimentSeq).padStart(4, '0')}`,
      project,
      name: body.name.trim(),
      ...(description ? { description } : {}),
      created_by: ME_SUB,
      created_by_label: ME_LABEL,
      created_at: wallClock(),
    };
    EXPERIMENTS.push(e);
    send(201, experimentView(e));
  });
}

/** `GET /experiments`: a project's (or every) open experiment, archived ones on asking, newest
 * activity first. */
function listExperiments({ query, send }) {
  const project = query.get('project');
  const flags = [boolParam(query, 'archived'), boolParam(query, 'mine')];
  const bad = flags.find((f) => f.error);
  if (bad) return send(...bad.error);
  const [archived, mine] = flags.map((f) => f.value);
  const experiments = EXPERIMENTS.filter(
    (e) =>
      (!project || e.project === project) &&
      (archived || e.archived_at == null) &&
      (!mine || isMine(e)),
  )
    .map(experimentView)
    .sort((a, b) => activityOf(b) - activityOf(a) || bytewise(b.id, a.id));
  send(200, { experiments });
}

/** The experiment a route names, or `null` once it answered the 404. */
function namedExperiment({ params, send }) {
  const e = EXPERIMENTS.find((x) => x.id === params.id);
  if (!e) send(404, `unknown experiment ${params.id}`);
  return e ?? null;
}

/** `GET /experiments/{id}`. */
function getExperiment(c) {
  const e = namedExperiment(c);
  if (e) c.send(200, experimentDetail(e));
}

/** `PUT /experiments/{id}`: rename, describe, archive or restore it. */
function updateExperiment(c) {
  const e = namedExperiment(c);
  if (!e) return;
  const { req, send } = c;
  readJson(req, send, (update) => {
    const refusal =
      (update.name != null ? nameRefusal(update.name) : null) ??
      descriptionRefusal(update.description);
    if (refusal) return send(400, refusal);
    // The experiment as it would be must not clash with a live one of the project (an archived
    // one clashes with none).
    const live = update.archived == null ? e.archived_at == null : !update.archived;
    const clash = live ? nameClash(update.name ?? e.name, e.project, e.id) : null;
    if (clash) return send(409, clash);
    if (update.name != null) e.name = update.name.trim();
    if ('description' in update) {
      const d = typeof update.description === 'string' ? update.description.trim() : '';
      if (d) e.description = d;
      else delete e.description;
    }
    if (update.archived === true && e.archived_at == null) e.archived_at = wallClock();
    if (update.archived === false) delete e.archived_at;
    send(200, experimentView(e));
  });
}

/** `GET /experiments/{id}/samples.csv`, in the downloader's time zone `tz` (absent or blank: UTC; a
 * zone it does not know: 400, as the backend answers). */
function experimentSamplesCsvRoute(c) {
  const e = namedExperiment(c);
  if (!e) return;
  const { query, res, send } = c;
  const tz = query.get('tz')?.trim() || 'UTC';
  try {
    new Intl.DateTimeFormat('en-US', { timeZone: tz });
  } catch {
    return send(400, `unknown time zone \`${tz}\` (an IANA name, such as \`Europe/Berlin\`)`);
  }
  const csv = experimentSamplesCsv(e, tz);
  if (csv.tooLarge) return send(413, csv.tooLarge);
  res.statusCode = 200;
  res.setHeader('content-type', 'text/csv; charset=utf-8');
  res.setHeader(
    'content-disposition',
    contentDisposition('attachment', samplesCsvFilename(e.name)),
  );
  res.end(csv.body);
}

/** `POST /runs/{id}/experiment`: put a run in an experiment, or take it out. */
function setRunExperiment({ req, params, send }) {
  readJsonOrNull(req, send, (body) => {
    // `experiment_id` is required, even to take the run out (`null`): a body without it (or not
    // JSON, or with it neither a string nor null) is refused, not read as "out of any".
    const field = body && typeof body === 'object' ? body.experiment_id : undefined;
    if (field !== null && typeof field !== 'string') return send(400, RUN_EXPERIMENT_BODY_ERROR);
    const runId = params.run_id;
    const r = RUNS.find((x) => x.run_id === runId);
    if (!r) return send(404, `unknown run ${runId}`);
    // Trimmed; null or blank takes it out.
    const id = field?.trim() || null;
    if (id !== null) {
      if (!r.project)
        return send(400, `run ${runId} has no project, so it cannot be put in an experiment`);
      const problem = membershipProblem(id, r.project);
      if (problem) return send(...problem);
      r.experiment_id = id;
    } else delete r.experiment_id;
    // The backend answers with the run's summary after the change.
    send(200, summary(r));
  });
}

export const EXPERIMENT_ROUTES = [
  ['GET', '/experiments', listExperiments],
  ['POST', '/experiments', createExperiment],
  ['GET', '/experiments/{id}', getExperiment],
  ['PUT', '/experiments/{id}', updateExperiment],
  ['GET', '/experiments/{id}/samples.csv', experimentSamplesCsvRoute],
  ['POST', '/runs/{run_id}/experiment', setRunExperiment],
];
