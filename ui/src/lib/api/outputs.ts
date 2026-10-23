// A run's outputs: paging them, one output, its download and view links and previews, and its
// FastQC and alignment views.

import type { components } from '$lib/generated/api';
import {
  API_PATHS,
  get,
  getOrNull,
  pathFor,
  requestJson,
  withQuery,
  type JsonBody,
} from './client';

export type RunOutputPage = JsonBody<'query_outputs'>;
export type RunOutput = RunOutputPage['outputs'][number];
type RunOutputQueryRequest = components['schemas']['RunOutputQueryRequest'];
export type OutputUrl = JsonBody<'download_output'>;
export type OutputPreview = JsonBody<'preview_output'>;

/**
 * A page of the objects a run registered, ordered by S3 URI. Asked with a POST body: a task key and
 * the cursor (an object's URI) can name a sample, and must not reach a request URL.
 */
export function listOutputs(
  runId: string,
  opts: {
    /** Only this task's outputs. */
    task?: string;
    /** Roles to include (`*` ⇒ every role); omitted ⇒ everything except work-dir intermediates. */
    roles?: string[];
    /** Only outputs under this folder of the run's output prefix (relative to it, ending in `/`). */
    prefix?: string;
    /** Keyset cursor from a prior page's `next_cursor`. */
    after?: string;
    limit?: number;
  } = {},
): Promise<RunOutputPage> {
  // A task key and a folder name a sample, and the cursor is an output's S3 URI: they go in the
  // body of the POST form, never in a request URL (which proxies and access logs keep).
  const body: RunOutputQueryRequest = {
    ...(opts.task ? { task: opts.task } : {}),
    ...(opts.roles && opts.roles.length > 0 ? { roles: opts.roles } : {}),
    ...(opts.prefix ? { prefix: opts.prefix } : {}),
    ...(opts.after ? { after: opts.after } : {}),
    ...(opts.limit != null ? { limit: opts.limit } : {}),
  };
  return requestJson<RunOutputPage>(pathFor(API_PATHS.outputsQuery, { run_id: runId }), {
    method: 'POST',
    body,
  });
}

/** What [`listAllOutputs`] read: the outputs, the server's `total` of matching outputs, the last
 * page read (its `enrichment_pending` is the freshest), and whether rows were left unread
 * (`truncated`: the row cap hit while the server had more). */
export interface AllOutputs {
  outputs: RunOutput[];
  total: number;
  last: RunOutputPage;
  truncated: boolean;
}

/**
 * Every page of a run's outputs matching `opts` ([`listOutputs`]), by keyset paging, until the last
 * page, `maxRows` rows, or a page `stop` says is enough (it sees each page as it arrives). The one
 * paging loop the outputs readers share.
 */
export async function listAllOutputs(
  runId: string,
  opts: Omit<Parameters<typeof listOutputs>[1] & object, 'after' | 'limit'> = {},
  {
    maxRows,
    pageSize = 2000,
    stop,
    list = listOutputs,
  }: {
    maxRows: number;
    pageSize?: number;
    stop?: (page: RunOutputPage) => boolean;
    /** The page reader (a test's stand-in). */
    list?: typeof listOutputs;
  },
): Promise<AllOutputs> {
  const outputs: RunOutput[] = [];
  let after: string | undefined;
  for (;;) {
    const page = await list(runId, { ...opts, after, limit: pageSize });
    outputs.push(...page.outputs);
    after = page.next_cursor ?? undefined;
    if (!after || stop?.(page)) return { outputs, total: page.total, last: page, truncated: false };
    if (outputs.length >= maxRows)
      return { outputs, total: page.total, last: page, truncated: true };
  }
}

/** One output of the run by id, shaped as in the list (e.g. a linked file past the listed page). */
export function getOutput(runId: string, outputId: string): Promise<RunOutput> {
  return get<RunOutput>(pathFor(API_PATHS.output, { run_id: runId, output_id: outputId }));
}

/**
 * A short-lived download URL for one output. The API is token-authenticated, so a plain link to it
 * would not carry the caller's credentials: fetch the URL, then navigate to it.
 */
export function getOutputDownload(runId: string, outputId: string): Promise<OutputUrl> {
  return get<OutputUrl>(pathFor(API_PATHS.outputDownload, { run_id: runId, output_id: outputId }));
}

/** A short-lived URL that shows an image, PDF or HTML output inline (its `viewer` says which). */
export function getOutputViewUrl(runId: string, outputId: string): Promise<OutputUrl> {
  return get<OutputUrl>(pathFor(API_PATHS.outputViewUrl, { run_id: runId, output_id: outputId }));
}

/** The start (≤ 1 MiB, decompressed when gzip) of a text or table output. */
export function getOutputPreview(runId: string, outputId: string): Promise<OutputPreview> {
  return get<OutputPreview>(
    pathFor(API_PATHS.outputPreview, { run_id: runId, output_id: outputId }),
  );
}

export type FastqcReport = JsonBody<'output_fastqc'>;
export type FastqcModule = FastqcReport['modules'][number];
export type AlignmentLinks = JsonBody<'alignment_view'>;
/** The reference of an {@link AlignmentLinks}: a bundle's FASTA or a hosted genome, by its `source`. */
export type AlignmentReference = components['schemas']['AlignmentReference'];

/**
 * The pipeline's own FastQC result for an output (its `_fastqc.zip`, the zip beside a
 * `_fastqc.html`, or the zip named after a FASTQ), as data; `null` when the run has none (404), so
 * the caller can show the HTML report instead. A zip that cannot be read (too large, or not a
 * FastQC zip) is a 422, thrown as an {@link ApiError}.
 */
export function getOutputFastqc(runId: string, outputId: string): Promise<FastqcReport | null> {
  return getOrNull<FastqcReport>(
    pathFor(API_PATHS.outputFastqc, { run_id: runId, output_id: outputId }),
  );
}

/**
 * What the genome browser needs to show a BAM/CRAM output: Range-readable links to it, its index
 * and its reference (the run's bundle, or `reference`, a bundle id), and where to open. The links
 * stop working at `expires_at` (or sooner: ask again on a 403). `locus: false` skips finding where
 * to open (the response has no `locus`), for new links to a file already open. A 409 (not an
 * alignment, or no index) is thrown as an {@link ApiError} with the server's reason.
 */
export function getAlignmentView(
  runId: string,
  outputId: string,
  reference?: string | null,
  { locus = true }: { locus?: boolean } = {},
): Promise<AlignmentLinks> {
  const q = new URLSearchParams();
  if (reference) q.set('reference', reference);
  if (!locus) q.set('locus', 'false');
  return get<AlignmentLinks>(
    withQuery(pathFor(API_PATHS.outputAlignmentView, { run_id: runId, output_id: outputId }), q),
  );
}
