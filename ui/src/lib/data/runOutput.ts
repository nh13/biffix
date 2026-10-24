// A file in an analysis's results folder, as that analysis's output: what the genome browser and
// the FastQC viewer open (they read a run's output by its id). Looked up by the file's own path as
// the outputs list's `prefix` (a URI prefix: the file, and any file whose name starts with its
// name, such as its index), so one small page almost always settles it. Says apart "the run
// registered no output there" (a file the engine wrote but did not report, or one a later run added
// to a shared folder) from "the lookup gave up" (more same-named outputs than it reads).

import { listAllOutputs, listOutputs, type RunOutput } from '#lib/api/index.js';

/** Outputs read per page, and at most, looking for one. */
const PAGE = 200;
const MAX_ROWS = 2000;

/** What the lookup found: the output, that the run recorded none at that path, or that it stopped
 * before it could tell. */
export type RunOutputLookup = { output: RunOutput } | { notRecorded: true } | { truncated: true };

/**
 * The output of run `runId` at `uri`, `rest` being its path under the run's results folder (as the
 * Data page has it). Rejects when the outputs cannot be read. `list` reads a page (a test's
 * stand-in).
 */
export async function runOutputAt(
  runId: string,
  rest: string,
  uri: string,
  list: typeof listOutputs = listOutputs,
): Promise<RunOutputLookup> {
  let output: RunOutput | undefined;
  const read = await listAllOutputs(
    runId,
    // Every role: a lookup by exact path is cheap, and a result is never missed for its role.
    { roles: ['*'], ...(rest ? { prefix: rest } : {}) },
    {
      maxRows: MAX_ROWS,
      pageSize: PAGE,
      stop: (page) => !!(output = page.outputs.find((o) => o.s3_uri === uri)),
      list,
    },
  );
  output ??= read.outputs.find((o) => o.s3_uri === uri);
  if (output) return { output };
  return read.truncated ? { truncated: true } : { notRecorded: true };
}
