// Workflows: the registered versions, the engines, what a launch usually costs, and a pipeline's
// Lab listing and a version's release.

import type { components } from '#lib/generated/api.js';
import {
  API_PATHS,
  DEVELOPER_ONLY,
  get,
  pathFor,
  requestJson,
  withQuery,
  type JsonBody,
} from './client';

export type WorkflowEstimate = JsonBody<'estimate_workflow'>;

/**
 * What a launch of `samples` samples usually costs and takes, and what one run of the workflow
 * usually takes (`typical`), from the workflow's past runs. Without `samples`, the typical run only.
 */
export function getWorkflowEstimate(
  versionId: string,
  samples?: number,
): Promise<WorkflowEstimate> {
  const params = new URLSearchParams(samples != null ? { samples: String(samples) } : {});
  return get<WorkflowEstimate>(withQuery(pathFor(API_PATHS.estimate, { id: versionId }), params));
}

export type WorkflowSummary = JsonBody<'list_workflows'>[number];
/** A version's entry in the Lab's pipeline catalog (its description, tags, results and inputs). */
export type CatalogEntry = components['schemas']['CatalogEntry'];
export type WorkflowDetail = JsonBody<'get_workflow'>;

export function listWorkflows(): Promise<WorkflowSummary[]> {
  return get<WorkflowSummary[]>(API_PATHS.workflows);
}

export type EngineSupport = JsonBody<'list_engines'>[number];

/** The engines the deployment runs and the releases a workflow may pin as its engine_version. */
export function listEngines(): Promise<EngineSupport[]> {
  return get<EngineSupport[]>(API_PATHS.engines);
}

export function getWorkflow(id: string): Promise<WorkflowDetail> {
  return get<WorkflowDetail>(pathFor(API_PATHS.workflow, { id }));
}

export type WorkflowListing = components['schemas']['WorkflowListing'];

/** Hide a pipeline from (or show it in) the Lab's listing of pipelines. Developers only. */
export function putWorkflowListing(
  project: string,
  name: string,
  hidden: boolean,
): Promise<WorkflowListing> {
  return requestJson<WorkflowListing>(API_PATHS.workflowListing, {
    method: 'PUT',
    body: { project, name, hidden } satisfies components['schemas']['WorkflowListingUpdateRequest'],
    forbidden: DEVELOPER_ONLY,
  });
}

export type WorkflowRelease = components['schemas']['WorkflowRelease'];
/**
 * Whether a version is released, and by whom and when: the one release shape the list, the
 * detail and a release change all carry.
 */
export type VersionRelease = components['schemas']['VersionRelease'];

/** Release a version (or, `false`, take the release back). Developers and admins only. */
export function putWorkflowRelease(id: string, released: boolean): Promise<WorkflowRelease> {
  return requestJson<WorkflowRelease>(pathFor(API_PATHS.workflowRelease, { id }), {
    method: 'PUT',
    body: { released } satisfies components['schemas']['WorkflowReleaseUpdateRequest'],
    forbidden: DEVELOPER_ONLY,
  });
}
