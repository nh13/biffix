// Experiments: named groups of a project's runs. Creating, renaming and changing a
// run's experiment need the launch capability; a 403 keeps the server's own message.

import type { components } from '#lib/generated/api.js';
import { API_PATHS, get, pathFor, request, requestJson, withQuery } from './client';

export type Experiment = components['schemas']['Experiment'];
export type ExperimentList = components['schemas']['ExperimentList'];
export type ExperimentDetail = components['schemas']['ExperimentDetail'];
export type ExperimentCreateRequest = components['schemas']['ExperimentCreateRequest'];
export type ExperimentUpdateRequest = components['schemas']['ExperimentUpdateRequest'];

/** The experiments, newest activity first: of one project, with the archived ones on asking, and
 * (`mine`) only the caller's: those they made or have an analysis in. */
export function listExperiments(
  opts: { project?: string | null; archived?: boolean; mine?: boolean } = {},
): Promise<ExperimentList> {
  const params = new URLSearchParams();
  if (opts.project) params.set('project', opts.project);
  if (opts.archived) params.set('archived', 'true');
  if (opts.mine) params.set('mine', 'true');
  return get<ExperimentList>(withQuery(API_PATHS.experiments, params));
}

/** Create an experiment; a name taken in the project is a 409 whose message names it. */
export function createExperiment(body: ExperimentCreateRequest): Promise<Experiment> {
  return requestJson<Experiment>(API_PATHS.experiments, { method: 'POST', body });
}

/** One experiment with its runs (newest first), the samples across them and one total cost. */
export function getExperiment(id: string): Promise<ExperimentDetail> {
  return get<ExperimentDetail>(pathFor(API_PATHS.experiment, { id }));
}

/** Every sample of every one of an experiment's runs (not only the newest the detail holds), as a
 * CSV file to save: one row per sample per run, each analysis named as the UI names it. */
export async function getExperimentSamplesCsv(id: string): Promise<Blob> {
  // Unnamed analyses are named by their start time, read in this browser's zone (each time with
  // the zone's rules for it, daylight saving included) as the UI shows it.
  const tz = Intl.DateTimeFormat().resolvedOptions().timeZone;
  const path = withQuery(
    pathFor(API_PATHS.experimentSamplesCsv, { id }),
    new URLSearchParams({ tz }),
  );
  return (await request(path, { accept: 'text/csv' })).blob();
}

/** Rename, describe, archive or restore an experiment; resolves to what the server now stores. */
export function updateExperiment(id: string, update: ExperimentUpdateRequest): Promise<Experiment> {
  return requestJson<Experiment>(pathFor(API_PATHS.experiment, { id }), {
    method: 'PUT',
    body: update,
  });
}

/** Put a run in an experiment of its project, or (`null`) take it out of the one it is in. */
export async function setRunExperiment(runId: string, experimentId: string | null): Promise<void> {
  const body: components['schemas']['RunExperimentSetRequest'] = { experiment_id: experimentId };
  await request(pathFor(API_PATHS.runExperiment, { run_id: runId }), {
    method: 'POST',
    body,
  });
}
