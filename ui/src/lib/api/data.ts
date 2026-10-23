// Data sources: what the launch form's file picker browses. Their errors carry the server's own
// message (e.g. which grant a source is missing).

import type { components } from '$lib/generated/api';
import { API_PATHS, get, requestJson, withQuery, type JsonBody } from './client';
import type { OutputPreview, OutputUrl } from './outputs';

export type DataSource = JsonBody<'list_sources'>[number];
export type DataFolderPage = JsonBody<'list_folder'>;
export type DataFile = DataFolderPage['files'][number];
export type DataFolder = DataFolderPage['folders'][number];
export type DataFindResult = JsonBody<'find_files'>;
export type DataHead = JsonBody<'head_objects'>['objects'][number];

export type Project = JsonBody<'list_projects'>[number];

/** The deploy's projects (each with its own bucket, data sources and upload setting). */
export function listProjects(): Promise<Project[]> {
  return get<Project[]>(API_PATHS.projects);
}

/**
 * The project a data read or upload names: absent (the deploy's default project) unless given.
 * `null`, `undefined` and `''` all mean "the default", so a caller passes what it has.
 */
export type ProjectRef = string | null | undefined;

/** `{ project }` when one is named, else nothing (the server then uses the default project). */
export function projectField(project: ProjectRef): { project?: string } {
  return project ? { project } : {};
}

/** Where inputs can be picked from in `project` (the default project's when absent). */
export function listDataSources(project?: ProjectRef): Promise<DataSource[]> {
  return get<DataSource[]>(
    withQuery(API_PATHS.dataSources, new URLSearchParams(projectField(project))),
  );
}

// Folder and file names can name a sample, so these reads send them in a POST body, never in the
// request URL (which access logs record).

/** One folder of a data source (`path` relative to its root; `''` is the root). */
export function listDataFolder(
  source: string,
  path: string,
  opts: { startsWith?: string; cursor?: string; project?: ProjectRef } = {},
): Promise<DataFolderPage> {
  const body: components['schemas']['DataListRequest'] = {
    source,
    path,
    ...projectField(opts.project),
  };
  if (opts.startsWith) body.starts_with = opts.startsWith;
  if (opts.cursor) body.cursor = opts.cursor;
  return requestJson<DataFolderPage>(API_PATHS.dataList, { method: 'POST', body });
}

/** Files under `path` (any depth) whose name contains `q`. */
export function findDataFiles(
  source: string,
  path: string,
  q: string,
  project?: ProjectRef,
): Promise<DataFindResult> {
  const body: components['schemas']['DataFindRequest'] = {
    source,
    path,
    q,
    ...projectField(project),
  };
  return requestJson<DataFindResult>(API_PATHS.dataFind, { method: 'POST', body });
}

/** A POST naming one file of a data source, as the file reads below send it. */
const postDataFile = <T>(route: string, source: string, path: string, project: ProjectRef) =>
  requestJson<T>(route, {
    method: 'POST',
    body: {
      source,
      path,
      ...projectField(project),
    } satisfies components['schemas']['DataFileRequest'],
  });

/** The start of one file in a data source, as text (decompressed when gzip). */
export function getDataPreview(
  source: string,
  path: string,
  project?: ProjectRef,
): Promise<OutputPreview> {
  return postDataFile<OutputPreview>(API_PATHS.dataPreview, source, path, project);
}

/** A short-lived URL showing an image, PDF or HTML file in a data source inline. */
export function getDataViewUrl(
  source: string,
  path: string,
  project?: ProjectRef,
): Promise<OutputUrl> {
  return postDataFile<OutputUrl>(API_PATHS.dataViewUrl, source, path, project);
}

/** A short-lived URL downloading a file in a data source. */
export function getDataDownloadUrl(
  source: string,
  path: string,
  project?: ProjectRef,
): Promise<OutputUrl> {
  return postDataFile<OutputUrl>(API_PATHS.dataDownloadUrl, source, path, project);
}

/** Whether each `s3://` URI exists; `exists` is absent when biffix cannot see the location. */
export async function headDataObjects(uris: string[], project?: ProjectRef): Promise<DataHead[]> {
  const body = await requestJson<{ objects: DataHead[] }>(API_PATHS.dataHead, {
    method: 'POST',
    body: { uris, ...projectField(project) } satisfies components['schemas']['DataHeadRequest'],
  });
  return body.objects;
}
