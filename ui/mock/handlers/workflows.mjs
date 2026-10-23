// Workflow routes (workflows.rs): the version list, a version's detail, estimate and release, the
// Lab listing's hidden pipelines, and the engines a registration may name.
import { DEFAULT_PROJECT } from '../fixtures/data.mjs';
import { RNASEQ_FORM } from '../fixtures/forms.mjs';
import { LAB_VERSION_ID } from '../fixtures/lab.mjs';
import { RUNS } from '../fixtures/runs.mjs';
import {
  BUILDING_VERSION,
  DERIVING_VERSION,
  LAB_VERSION,
  PENDING_VERSION,
  RELEASES,
  RNASEQ_ABOUT,
  TYPICAL_RUNS,
  WORKFLOWS,
} from '../fixtures/workflows.mjs';
import { now } from '../lib/clock.mjs';
import { missingField, readJson } from '../lib/http.mjs';
import { boolParam, intParam } from '../lib/query.mjs';
import { HIDDEN, ME, ME_LABEL, ME_SUB } from '../state.mjs';

// nf-core/rnaseq's listed version: the one with a form, an about page and an estimate.
const RNASEQ_VERSION_ID = 'aabbccddeeff:v1';

/** A version's current release (`VersionRelease::of`): what the list and the detail carry. */
function releaseFields(id) {
  const current = (RELEASES.get(id) ?? [])[0];
  if (!current?.released) return { release: { released: false } };
  return {
    release: {
      released: true,
      at_micros: current.at_micros,
      ...(current.by ? { by: current.by } : {}),
      ...(current.by_label ? { by_label: current.by_label } : {}),
    },
  };
}
/** A version's release as `PUT /workflows/{id}/release` answers it (see `WorkflowRelease::of`). */
function releaseOf(id) {
  // The mock's caller is a developer.
  return {
    version_id: id,
    ...releaseFields(id),
    history: RELEASES.get(id) ?? [],
    can_change: true,
  };
}
const catalogKey = (project, name) => `${project}\u0000${name}`;
// `catalog_results`: the names of the key results a form declares, in order, each once.
function catalogResults(form) {
  const names = [];
  for (const r of form?.reports ?? []) {
    const name = r.name?.trim();
    if (name && !names.includes(name)) names.push(name);
  }
  return names;
}
// `catalog_inputs`: its samplesheets and the required, shown file inputs its reference dropdown does
// not fill, in plain words, then that dropdown; each once, at most four.
function catalogInputs(form) {
  const humanize = (n) => {
    const spaced = n.replace(/[_-]/g, ' ').trim();
    return spaced.charAt(0).toUpperCase() + spaced.slice(1);
  };
  const inputs = [];
  // Fields the reference dropdown fills are said once, as the reference.
  const filled = form?.references?.fill ?? {};
  for (const f of form?.fields ?? []) {
    const sheet = f.samplesheet != null;
    const file = f.is_path_input && f.required && !f.is_output && !f.hidden;
    if ((!sheet && !file) || Object.hasOwn(filled, f.name ?? '')) continue;
    const label = f.label?.trim() || humanize(f.name ?? '');
    if (!label) continue;
    const text = sheet
      ? `${label} (one row per sample)`
      : f.suffixes?.length
        ? `${label} (${f.suffixes.join(', ')})`
        : label;
    if (!inputs.includes(text)) inputs.push(text);
  }
  // At most four, the reference always among them (the field inputs give up the last slot).
  const ref = form?.references?.label?.trim();
  if (ref && !inputs.includes(ref)) return [...inputs.slice(0, 3), ref];
  return inputs.slice(0, 4);
}
// A listed version as the server lists it: its run count and newest run (runs of that version,
// launches too), its catalog (the frozen form's `catalog`, the description falling back to the
// summary), and whether a developer hid it from the Lab listing.
function versionView(v) {
  const runs = RUNS.filter((r) => r.workflow === v.id);
  const last = Math.max(...runs.map((r) => r.submitted_at ?? -Infinity));
  const form = formOf(v.id);
  const frozen = form?.catalog;
  const description = frozen?.description ?? v.summary;
  const results = catalogResults(form);
  const inputs = catalogInputs(form);
  const catalog =
    description || frozen?.tags?.length || frozen?.verified || results.length || inputs.length
      ? {
          ...(description ? { description } : {}),
          tags: frozen?.tags ?? [],
          verified: frozen?.verified ?? false,
          ...(frozen?.contact ? { contact: frozen.contact } : {}),
          ...(results.length ? { results } : {}),
          ...(inputs.length ? { inputs } : {}),
        }
      : undefined;
  const title = workflowTitleOf(v.id);
  return {
    ...v,
    ...(title ? { title } : {}),
    run_count: runs.length,
    ...(Number.isFinite(last) ? { last_run_at: last } : {}),
    ...(catalog ? { catalog } : {}),
    hidden: HIDDEN.has(catalogKey(v.project, v.name)),
    ...releaseFields(v.id),
    // The pipeline's typical run, as `list_workflows` attaches it to every version of it.
    ...(TYPICAL_RUNS[v.name] ? { typical: TYPICAL_RUNS[v.name] } : {}),
  };
}

// WorkflowVersionDetail = WorkflowVersionSummary + entrypoint/source/form_schema/target.
function workflowDetail(v) {
  if (v.id === RNASEQ_VERSION_ID)
    return { ...workflowDetailBase(v), form_schema: RNASEQ_FORM, about_markdown: RNASEQ_ABOUT };
  return workflowDetailBase(v);
}
// Its form declares the key results its analyses write (`reports:`, frozen with the form).
const labDetail = () => ({
  ...workflowDetailBase(LAB_VERSION),
  form_schema: RNASEQ_FORM,
});
// The rnaseq form a version was frozen with, when it has one (rnaseq's own version, and the Lab
// analyses' version of it).
const formOf = (id) =>
  id === RNASEQ_VERSION_ID || id === LAB_VERSION_ID ? RNASEQ_FORM : undefined;
// The pipeline's friendly title (its frozen form's `title`), when it has one: the run list's
// `workflow_title`, the version list's `title`.
export const workflowTitleOf = (id) => formOf(id)?.title;
// The author's `catalog:` a version was frozen with (its form's), when it has one.
export const frozenCatalog = (id) => formOf(id)?.catalog;
function pendingDetail() {
  return {
    ...workflowDetailBase(PENDING_VERSION),
    state: 'pending',
    not_ready: ['image `fastqc` is being imported'],
    images: [
      {
        name: 'default',
        declared_ref: 'ubuntu:24.04',
        source_ref: 'docker.io/library/ubuntu:24.04@sha256:' + '3'.repeat(64),
        state: 'ready',
        image:
          '123456789012.dkr.ecr.us-east-1.amazonaws.com/biffix-workers@sha256:' + '3'.repeat(64),
        archs: ['amd64', 'arm64'],
        capabilities: { bash: true, libc: 'glibc' },
        archived: true,
        keep_tag: 'biffix-keep-333333333333',
      },
      {
        name: 'fastqc',
        declared_ref: 'quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0',
        source_ref: 'quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0@sha256:' + '4'.repeat(64),
        state: 'building',
        archs: [],
        capabilities: {},
        archived: false,
        logs_url: 'https://console.aws.amazon.com/codesuite/codebuild/projects/x/build/y',
      },
    ],
  };
}
function derivingDetail() {
  const repo = '123456789012.dkr.ecr.us-east-1.amazonaws.com/biffix-workers';
  return {
    ...workflowDetailBase(DERIVING_VERSION),
    state: 'pending',
    not_ready: ['image `default`: adding the Snakemake 9.27.0 runtime'],
    images: [
      {
        name: 'default',
        declared_ref: 'quay.io/biocontainers/samtools:1.21--h50ea8bc_0',
        source_ref: 'quay.io/biocontainers/samtools:1.21--h50ea8bc_0@sha256:' + '5'.repeat(64),
        state: 'building',
        image: `${repo}@sha256:` + '5'.repeat(64),
        archs: ['amd64'],
        capabilities: { bash: true, libc: 'glibc' },
        archived: true,
        keep_tag: 'biffix-keep-555555555555',
        derived_state: 'building',
      },
      {
        name: 'bwa',
        declared_ref: 'quay.io/biocontainers/bwa:0.7.18--he4a0461_1',
        source_ref: 'quay.io/biocontainers/bwa:0.7.18--he4a0461_1@sha256:' + '6'.repeat(64),
        state: 'ready',
        image: `${repo}@sha256:` + '6'.repeat(64),
        archs: ['amd64'],
        capabilities: { bash: true, libc: 'glibc' },
        archived: true,
        keep_tag: 'biffix-keep-666666666666',
        derived_state: 'ready',
        derived_image: `${repo}@sha256:` + '7'.repeat(64),
      },
    ],
  };
}
function buildingDetail() {
  const repo = '123456789012.dkr.ecr.us-east-1.amazonaws.com/biffix-workers';
  const log = 'https://console.aws.amazon.com/codesuite/codebuild/projects/x/build/arm';
  return {
    ...workflowDetailBase(BUILDING_VERSION),
    registration_inputs: {
      source_mode: 's3',
      cli_version: '0.1.0',
      overrides: { engine_version: '9.27.0' },
      reregistered_from: RNASEQ_VERSION_ID,
    },
    state: 'failed',
    not_ready: [`image \`default\` failed to build (arm64): FAILED: BUILD (build log: ${log})`],
    images: [
      {
        name: 'default',
        kind: 'pixi',
        declared_ref: 'default',
        source_ref: 'pixi:env',
        state: 'failed',
        archs: [],
        capabilities: {},
        archived: false,
        reason: 'FAILED: BUILD',
        logs_url: log,
        generated_dockerfile:
          '# Generated by biffix (pixi generator 1).\nFROM ghcr.io/prefix-dev/pixi:0.71.0 AS pixi\n',
        arch_builds: [
          { arch: 'amd64', state: 'succeeded' },
          { arch: 'arm64', state: 'failed', reason: 'FAILED: BUILD', logs_url: log },
        ],
      },
      {
        name: 'align',
        kind: 'dockerfile',
        declared_ref: 'align',
        source_ref: 'dockerfile:docker/align/Dockerfile',
        state: 'ready',
        image: `${repo}@sha256:` + '8'.repeat(64),
        archs: ['amd64'],
        capabilities: { bash: true, libc: 'glibc' },
        archived: true,
        keep_tag: 'biffix-keep-888888888888',
        derived_state: 'ready',
        derived_image: `${repo}@sha256:` + '9'.repeat(64),
        arch_builds: [{ arch: 'amd64', state: 'succeeded' }],
      },
    ],
  };
}

// The engine runtime a registered version runs on (GET /workflows/{id} `runtime`): built and ready.
function mockRuntime(v) {
  const key = `${v.engine}-${v.engine_version}`.replace(/[^a-z0-9]/gi, '').padEnd(64, '0');
  return {
    key,
    recipe: { engine: v.engine, engine_version: v.engine_version, recipe_version: 2 },
    state: 'ready',
    ready: true,
    builds: 1,
    coordinator_image: `123456789012.dkr.ecr.us-east-1.amazonaws.com/biffix-coordinator@sha256:${key}`,
    job_definition: `arn:aws:batch:us-east-1:123456789012:job-definition/biffix-mock-coordinator-rt-${key.slice(0, 12)}:1`,
  };
}
// WorkflowVersionDetail: the version's facts without its list-only summary line.
function workflowDetailBase(v) {
  const { summary: _summary, ...facts } = v;
  return {
    ...facts,
    ...releaseFields(v.id),
    // The mock's caller is a developer.
    can_release: true,
    // The engine's default entrypoint, unless the version names its own.
    entrypoint: v.entrypoint ?? (v.engine === 'snakemake' ? 'Snakefile' : 'main.nf'),
    target: null,
    retention_days: 30,
    build: Number(v.id.split(':')[2] ?? 0) || 0,
    registration_inputs: { source_mode: 's3', cli_version: '0.1.0' },
    runtime: v.engine_version ? mockRuntime(v) : undefined,
    state: 'ready',
    not_ready: [],
    images: [],
    source: { kind: 's3', bucket: 'biffix-artifacts', key: `workflows/${v.id}.tar.gz` },
    source_content_hash: v.id.split(':')[1] ?? v.id,
    form_schema: {
      fields: [
        {
          name: 'reads',
          widget: 's3_file',
          required: true,
          is_output: false,
          is_secret: false,
          is_path_input: true,
          description: 'Input FASTQ',
        },
        {
          name: 'out_dir',
          widget: 's3_dir',
          required: false,
          is_output: true,
          is_secret: false,
          is_path_input: true,
        },
      ],
      warnings: [],
    },
  };
}

// A version by id, as the registry holds it (its detail, `detailFor`), and whether it can run now;
// one only the fixtures' runs mention launches with the provenance its runs record.
export function versionById(id) {
  const detail = detailFor(id);
  if (!detail) return null;
  const ready = detail.state === 'ready' && !detail.not_ready.length;
  const fixture = [LAB_VERSION, PENDING_VERSION, DERIVING_VERSION, BUILDING_VERSION, ...WORKFLOWS];
  if (fixture.some((v) => v.id === id)) return { ...detail, ready };
  return { ...detail, ready, provenance: RUNS.find((x) => x.workflow === id).provenance };
}

/** `GET /engines`: the engine versions a registration may name. */
function listEngines({ send }) {
  send(200, [
    { engine: 'snakemake', min_version: '9.0.0', below_version: '10.0.0' },
    {
      engine: 'nextflow',
      min_version: '25.10.4',
      below_version: '26.0.0',
      releases: ['25.10.4'],
    },
  ]);
}

/** `GET /workflows?released=true|false`: only released (or only developer) versions. Anything
 * else is the backend's `Query<ListWorkflowsQuery>` refusal. */
function listWorkflows({ query, send }) {
  const released = boolParam(query, 'released');
  if (released.error) return send(...released.error);
  const all = WORKFLOWS.map(versionView);
  send(200, query.has('released') ? all.filter((v) => v.release.released === released.value) : all);
}

/** `PUT /workflows/listing`: hide or show a pipeline in the Lab listing, as `put_workflow_listing`
 * does. */
function putWorkflowListing({ req, send }) {
  readJson(req, send, ({ project, name, hidden }) => {
    if (!WORKFLOWS.some((w) => w.project === project && w.name === name))
      return send(404, `no workflow \`${name}\` is registered in project \`${project}\``);
    if (hidden) HIDDEN.add(catalogKey(project, name));
    else HIDDEN.delete(catalogKey(project, name));
    send(200, { project, name, hidden, updated_at_micros: Date.now() * 1000, updated_by: ME });
  });
}

/** `PUT /workflows/{id}/release`: release (or un-release) a version, as `put_workflow_release`
 * does: only a change is recorded, newest first. */
function putWorkflowRelease({ req, params, send }) {
  const { id } = params;
  if (!detailFor(id)) return send(404, `no workflow version \`${id}\` is registered`);
  readJson(req, send, ({ released }) => {
    // axum's `Json<WorkflowReleaseUpdateRequest>` refuses a missing or non-boolean `released`.
    if (typeof released !== 'boolean')
      return send(
        422,
        released === undefined
          ? missingField('released')
          : 'released: invalid type, expected a boolean',
      );
    const history = RELEASES.get(id) ?? [];
    if ((history[0]?.released ?? false) !== released)
      RELEASES.set(id, [
        // The backend records the caller's bare subject.
        { released, at_micros: Date.now() * 1000, by: ME_SUB, by_label: ME_LABEL },
        ...history,
      ]);
    send(200, releaseOf(id));
  });
}

/** `GET /workflows/{id}/estimate`: the rnaseq workflow has history ($2–$3 a sample, ~2.5 h);
 * others have none. A typical run (any sample count) by workflow name: rnaseq, the lab's rnaseq and
 * sarek have one. */
function getWorkflowEstimate({ params, query, send }) {
  const { id } = params;
  const version = versionById(id);
  // An unknown version is a bare 404, as the registry read answers.
  if (!version) return send(404);
  // `samples` is a u32.
  const samples = intParam(query, 'samples', 0, false, 0);
  if (samples.error) return send(...samples.error);
  const n = samples.value;
  const typical = TYPICAL_RUNS[version.name ?? ''];
  send(200, {
    ...(id === RNASEQ_VERSION_ID && n > 0
      ? {
          estimate: {
            usd_median: 2.5 * n,
            usd_low: 2 * n,
            usd_high: 3 * n,
            usd_per_sample: 2.5,
            hours_median: 2.5,
            runs: 6,
          },
        }
      : {}),
    ...(typical ? { typical } : {}),
  });
}

/**
 * A registered version's detail, as `GET /workflows/{id}` serves it, or `null` for an id the
 * registry does not have: the one set of registered versions every version route reads (a listed
 * one, one reached only by id, or one only the fixtures' runs mention).
 */
function detailFor(id) {
  if (id === PENDING_VERSION.id) return pendingDetail();
  if (id === DERIVING_VERSION.id) return derivingDetail();
  if (id === BUILDING_VERSION.id) return buildingDetail();
  if (id === LAB_VERSION_ID) return labDetail();
  const v = WORKFLOWS.find((w) => w.id === id);
  if (v) return workflowDetail(v);
  // A version only a run mentions: a minimal detail with an empty form.
  const r = RUNS.find((x) => x.workflow === id);
  return r
    ? {
        ...workflowDetailBase({
          id,
          name: r.workflow_name,
          project: r.project ?? DEFAULT_PROJECT,
          engine: r.engine,
          engine_version: '',
          git_sha: id.split(':')[0],
          image_digest: '',
          queue_classes: [],
          created_at_micros: (now - 30 * 86400) * 1_000_000,
        }),
        form_schema: { fields: [], warnings: [] },
      }
    : null;
}

/** `GET /workflows/{id}`. */
function getWorkflow({ params, send }) {
  const detail = detailFor(params.id);
  return detail ? send(200, detail) : send(404);
}

export const WORKFLOW_ROUTES = [
  ['GET', '/engines', listEngines],
  ['GET', '/workflows', listWorkflows],
  ['PUT', '/workflows/listing', putWorkflowListing],
  ['GET', '/workflows/{id}', getWorkflow],
  ['GET', '/workflows/{id}/estimate', getWorkflowEstimate],
  ['PUT', '/workflows/{id}/release', putWorkflowRelease],
];
