// A run's output routes (outputs.rs, objects.rs): the listing, one output, its preview, view and
// download links, its FastQC data and its alignment view.
import { NO_CORS_OUTPUTS, OUTPUTS, PREVIEWS, VIEW_URLS } from '../fixtures/outputs.mjs';
import { REFERENCES } from '../fixtures/references.mjs';
import { RUNS } from '../fixtures/runs.mjs';
import { outputPrefixOf } from '../lib/buckets.mjs';
import { fastqcFor } from '../lib/fastqc.mjs';
import { readJson } from '../lib/http.mjs';
import { downloadLink, previewBody } from '../lib/object-reads.mjs';
import { badLimit, bytewise, clampInt } from '../lib/paging.mjs';
import { ORIGIN } from '../state.mjs';

/**
 * `GET …/alignment-view` as alignment_view answers: 409 unless a BAM/CRAM with an index beside it
 * (`x.bam.bai`, `x.bai`, `x.bam.csi`; `x.cram.crai`, `x.crai` — registered ones only: the mock has
 * no bucket to HEAD); the reference from `?reference=` (404 when unknown) or the run's bundle — a
 * hosted genome when the bundle names one, else its FASTA — or none (with a note when the bundle
 * has no FASTA); the first contig with reads, unless `?locus=false` (renewing links only);
 * links for an hour.
 */
function alignmentView(run, rows, o, referenceId, { noCors, locus }, send) {
  if (!/\.(bam|cram)$/i.test(o.name)) return send(409, `${o.name} is not a BAM or CRAM file`);
  const cram = /\.cram$/i.test(o.name);
  const stem = o.path.slice(0, o.path.lastIndexOf('.'));
  const candidates = cram
    ? [`${o.path}.crai`, `${stem}.crai`]
    : [`${o.path}.bai`, `${stem}.bai`, `${o.path}.csi`];
  const index = candidates.map((path) => rows.find((x) => x.path === path)).find(Boolean);
  if (!index)
    return send(
      409,
      `${o.name} has no index (${cram ? '.crai' : '.bai or .csi'}) beside it; the viewer needs one`,
    );
  const bundleId = referenceId ?? run.reference_bundle?.id ?? null;
  const bundle = bundleId ? REFERENCES.find((b) => b.id === bundleId) : null;
  if (referenceId && !bundle) return send(404, `no reference bundle ${referenceId}`);
  const off = noCors || NO_CORS_OUTPUTS.has(o.path);
  const file = (name) => `${ORIGIN}/mock-files/igv/${name}${off ? '?mockCors=off' : ''}`;
  const notes = [];
  let reference = null;
  if (bundle?.igv_genome) {
    reference = {
      source: 'igv_genome',
      genome: bundle.igv_genome,
      bundle_id: bundle.id,
      display_name: bundle.display_name,
    };
  } else if (bundle?.uris.fasta) {
    reference = {
      source: 'bundle',
      bundle_id: bundle.id,
      display_name: bundle.display_name,
      fasta_url: file('tiny.fa'),
      fai_url: file('tiny.fa.fai'),
    };
    if (bundle.uris.gtf)
      notes.push(
        `The genes (${bundle.uris.gtf.split('/').pop()}) are too large to load without an index; add a tabix index to show them.`,
      );
  } else if (bundle) {
    notes.push(`reference bundle ${bundle.id} has no fasta the viewer can read`);
  }
  return send(200, {
    alignment: {
      name: o.name,
      format: cram ? 'cram' : 'bam',
      url: file('tiny.bam'),
      index_url: file('tiny.bam.bai'),
    },
    ...(reference ? { reference } : {}),
    ...(locus ? { locus: 'chrB:9001-11000' } : {}),
    expires_at: Date.now() / 1000 + 3600,
    bucket_cors: off ? 'unknown' : 'configured',
    notes,
  });
}

/**
 * A run's outputs (`POST …/outputs/query`, the filters in a JSON body so a task key never rides in
 * a URL), as outputs.rs: `null` (404) for an unknown run; ordered by S3 URI (bytewise); `prefix` a
 * path prefix under the run's output prefix (a folder, or a file's own path); keyset paging on the
 * URI (`after`, `limit` 1..=2000 default 500), with `next_cursor` when a page is full.
 */
function outputList(runId, { roles, task, prefix, after, limit }) {
  const run = RUNS.find((r) => r.run_id === runId);
  if (!run) return null;
  const base = outputPrefixOf(run);
  const size = clampInt(limit, 500, 1, 2000);
  const rows = (OUTPUTS[runId] ?? [])
    .filter(
      (o) =>
        (roles && roles.length > 0
          ? roles.includes('*') || roles.includes(o.role)
          : o.role !== 'intermediate') &&
        (!task || o.task_key === task) &&
        (!prefix || o.s3_uri.startsWith(`${base}${prefix}`)),
    )
    .sort((a, b) => bytewise(a.s3_uri, b.s3_uri));
  const page = rows.filter((o) => !after || o.s3_uri > after).slice(0, size);
  return {
    outputs: page,
    total: rows.length,
    output_prefix: base,
    ...(page.length === size ? { next_cursor: page[page.length - 1].s3_uri } : {}),
    enrichment_pending: 0,
  };
}

/** `POST /runs/{run_id}/outputs/query`. */
function queryOutputs({ req, params, send }) {
  readJson(req, send, (body) => {
    if (clampInt(body.limit, 500, 1, 2000) == null) return send(...badLimit(body.limit, true));
    const list = outputList(params.run_id, body);
    return list ? send(200, list) : send(404);
  });
}

/** One output of a run by its id: one of the run's outputs (its own, or one it reused from a run
 * it resumed); `undefined` for an unknown output, `null` for an unknown run. */
const findOutput = ({ params }) =>
  RUNS.some((r) => r.run_id === params.run_id)
    ? (OUTPUTS[params.run_id] ?? []).find((x) => x.id === params.output_id)
    : null;
/** outputs.rs `servable_output`: an output a read may serve (its object), or `null` once it
 * answered: an unknown run is a bare 404, an unknown output of it says so, and one biffix does
 * not serve is a 403. */
function servable(c) {
  const o = findOutput(c);
  if (o === null) c.send(404);
  else if (!o) c.send(404, 'no such output of this run');
  else if (!o.servable)
    c.send(
      403,
      'biffix does not serve this output (it is outside the project bucket, or a staged workflow source)',
    );
  return o?.servable ? o : null;
}
// Reading an output's object that is gone (runs/outputs.rs `aws_error`).
const NOT_IN_S3 = "the output's object is not in S3";

/** `GET /runs/{run_id}/outputs/{output_id}`: a bare 404 for an unknown run or output. */
function getOutput(c) {
  const o = findOutput(c);
  return o ? c.send(200, o) : c.send(404);
}

/** `GET …/outputs/{output_id}/preview`. */
function previewOutput(c) {
  const o = servable(c);
  if (!o) return;
  // A preview reads the object, which may be gone.
  if (o.exists === false) return c.send(404, NOT_IN_S3);
  c.send(200, previewBody(PREVIEWS[o.path]));
}

/** `GET …/outputs/{output_id}/view-url`. */
function viewOutputUrl(c) {
  const o = servable(c);
  if (!o) return;
  const viewUrl = VIEW_URLS[o.path];
  if (!viewUrl) return c.send(409, `${o.name} cannot be shown in the browser; download it`);
  c.send(200, {
    url: viewUrl,
    expires_at: Date.now() / 1000 + 600,
    filename: o.name,
    content_type: o.viewer === 'html' ? 'text/html; charset=utf-8' : 'image/svg+xml',
  });
}

/** `GET …/outputs/{output_id}/download`. */
function downloadOutput(c) {
  const o = servable(c);
  if (o) c.send(200, downloadLink(o.name));
}

/** `GET …/outputs/{output_id}/fastqc`: an output's FastQC data (404 when the run has no zip for
 * it). */
function outputFastqc(c) {
  const o = servable(c);
  if (!o) return;
  const report = fastqcFor(OUTPUTS[c.params.run_id] ?? [], o);
  return report ? c.send(200, report) : c.send(404, `no FastQC result for ${o.name}`);
}

/** `GET …/outputs/{output_id}/alignment-view`. */
function outputAlignmentView(c) {
  const o = servable(c);
  if (!o) return;
  const run = RUNS.find((r) => r.run_id === c.params.run_id);
  const rows = OUTPUTS[run.run_id] ?? [];
  const noCors = c.query.get('mockCors') === 'off';
  const locus = c.query.get('locus') !== 'false';
  alignmentView(run, rows, o, c.query.get('reference'), { noCors, locus }, c.send);
}

export const OUTPUT_ROUTES = [
  ['POST', '/runs/{run_id}/outputs/query', queryOutputs],
  ['GET', '/runs/{run_id}/outputs/{output_id}', getOutput],
  ['GET', '/runs/{run_id}/outputs/{output_id}/preview', previewOutput],
  ['GET', '/runs/{run_id}/outputs/{output_id}/view-url', viewOutputUrl],
  ['GET', '/runs/{run_id}/outputs/{output_id}/download', downloadOutput],
  ['GET', '/runs/{run_id}/outputs/{output_id}/fastqc', outputFastqc],
  ['GET', '/runs/{run_id}/outputs/{output_id}/alignment-view', outputAlignmentView],
];
