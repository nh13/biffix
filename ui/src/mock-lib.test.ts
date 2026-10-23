// The e2e mock's own logic, beside what `mock-contract.test.ts` checks over HTTP: the helpers in
// `mock/lib/` that mirror a backend function, held to that function's own test cases, and the
// handlers' behavior over time, which a request alone cannot show.

import { afterEach, describe, expect, it, vi } from 'vitest';

/** A mock module, imported as the mock imports it (plain ESM, outside the app's build). */
const mockModule = async <T>(path: string): Promise<T> =>
  (await import(/* @vite-ignore */ new URL(`../mock/${path}`, import.meta.url).href)) as T;

type Send = (status: number, body?: unknown) => void;
type Handler = (c: Record<string, unknown>) => void;

afterEach(() => {
  vi.useRealTimers();
});

describe('the mock’s helpers that mirror the backend', () => {
  it('matches a report glob as glob.rs does', async () => {
    const { globMatches } = await mockModule<{
      globMatches: (pattern: string, path: string) => boolean;
    }>('lib/glob.mjs');
    // glob.rs `a_star_stays_in_its_segment_and_a_double_star_crosses`.
    const cases: [string, string, boolean][] = [
      ['multiqc/multiqc_report.html', 'multiqc/multiqc_report.html', true],
      ['multiqc/*.html', 'multiqc/multiqc_report.html', true],
      ['*.html', 'multiqc/multiqc_report.html', false],
      ['**/*.html', 'multiqc/multiqc_report.html', true],
      ['**/*.html', 'top.html', true],
      ['reports/**/summary.pdf', 'reports/a/b/summary.pdf', true],
      ['reports/**/summary.pdf', 'reports/summary.pdf', true],
      ['qc/sample_?.html', 'qc/sample_1.html', true],
      ['qc/sample_?.html', 'qc/sample_12.html', false],
      ['qc/*.html', 'qc/a.htm', false],
    ];
    for (const [pattern, path, want] of cases)
      expect(globMatches(pattern, path), `${pattern} ~ ${path}`).toBe(want);
  });

  it('classifies a file and quotes a CSV field as viewer_of and csv.rs do', async () => {
    const { viewerOf } = await mockModule<{ viewerOf: (path: string) => string | null }>(
      'lib/viewer.mjs',
    );
    const { csvField } = await mockModule<{ csvField: (raw: string) => string }>('lib/csv.mjs');
    // viewer.rs `viewer_by_name`.
    const viewers: [string, string | null][] = [
      ['metrics.tsv', 'table'],
      ['x.umi_grouping.metrics', 'table'],
      ['calls.vcf.gz', 'table'],
      ['Data.CSV', 'table'],
      ['run.log', 'text'],
      ['reads.fastq.gz', 'text'],
      ['plot.png', 'image'],
      ['plot.svg', 'image'],
      ['report.pdf', 'pdf'],
      ['multiqc_report.html', 'html'],
      ['report.html.gz', null],
      ['aligned.bam', null],
      ['aligned.bam.bai', null],
      ['README', null],
    ];
    for (const [name, viewer] of viewers) expect(viewerOf(name), name).toBe(viewer);
    // csv.rs's `field` cases.
    const fields: [string, string][] = [
      ['S1', 'S1'],
      ['', ''],
      ['Café 鼠', 'Café 鼠'],
      ['5', '5'],
      ['Doe, Jane', '"Doe, Jane"'],
      ['a"b', '"a""b"'],
      ['two\nlines', '"two\nlines"'],
      ['cr\rhere', '"cr\rhere"'],
      ["'quoted", "''quoted"],
    ];
    for (const [raw, field] of fields) expect(csvField(raw), raw).toBe(field);
  });

  it('words a download’s Content-Disposition as content_disposition does', async () => {
    const { contentDisposition } = await mockModule<{
      contentDisposition: (kind: string, filename: string) => string;
    }>('lib/csv.mjs');
    // object_view.rs `content_disposition_sanitizes_and_encodes`, and a name with attr-chars that
    // `urlencoding` still encodes.
    const cases: [string, string][] = [
      ['a.bam', 'attachment; filename="a.bam"'],
      [
        'we"ird\\name\n.txt',
        `attachment; filename="weirdname.txt"; filename*=UTF-8''we%22ird%5Cname%0A.txt`,
      ],
      ['résumé.pdf', `attachment; filename="r_sum_.pdf"; filename*=UTF-8''r%C3%A9sum%C3%A9.pdf`],
      ['""', `attachment; filename="download"; filename*=UTF-8''%22%22`],
      [
        'é!#$&+^`|.csv',
        `attachment; filename="_!#$&+^\`|.csv"; filename*=UTF-8''%C3%A9%21%23%24%26%2B%5E%60%7C.csv`,
      ],
    ];
    for (const [name, header] of cases) expect(contentDisposition('attachment', name)).toBe(header);
  });

  it('finds a FASTQ’s FastQC zip as fastq_zip does', async () => {
    const { fastqcFor } = await mockModule<{
      fastqcFor: (rows: Record<string, string>[], o: Record<string, string>) => unknown;
    }>('lib/fastqc.mjs');
    const out = (path: string, role = 'output') => ({
      id: path,
      path,
      name: path.split('/').pop()!,
      role,
    });
    const raw = out('reads/S1.fq.gz');
    const trimmed = out('reads/trimmed/S1.fq.gz');
    const scratch = out('scratch/X.fq.gz', 'intermediate');
    const rows = [
      raw,
      trimmed,
      out('reads/trimmed/S1_fastqc.zip'),
      scratch,
      out('scratch/X_fastqc.zip'),
    ];
    // The trimmed reads' zip is theirs, not the untrimmed reads' above them.
    expect(fastqcFor(rows, trimmed)).toMatchObject({ output_id: 'reads/trimmed/S1_fastqc.zip' });
    expect(fastqcFor(rows, raw)).toBeNull();
    // A FASTQ of a role the lookup does not count has none.
    expect(fastqcFor(rows, scratch)).toBeNull();
  });

  it('picks a resume’s own MultiQC report before the resumed run’s', async () => {
    const { shallowestMultiqc } = await mockModule<{
      shallowestMultiqc: (rows: Record<string, string>[]) => { s3_uri: string } | undefined;
    }>('lib/multiqc.mjs');
    const report = (s3_uri: string, reused_from?: string) => ({
      s3_uri,
      name: 'multiqc_report.html',
      ...(reused_from ? { reused_from } : {}),
    });
    const picked = shallowestMultiqc([
      report('s3://b/r/multiqc_report.html', 'run_before'),
      report('s3://b/r/v2/qc/multiqc_report.html'),
      report('s3://b/r/v2/a/deeper/multiqc_report.html'),
    ]);
    expect(picked?.s3_uri).toBe('s3://b/r/v2/qc/multiqc_report.html');
  });

  it('reads a task’s sample and orders the steps as sample_progress does', async () => {
    const { sampleGrid } = await mockModule<{
      sampleGrid: (run: Record<string, unknown>) => {
        steps: { group: string; scope: string }[];
        samples: { id: string }[];
        unattributed: number;
      };
    }>('lib/sample-grid.mjs');
    const task = (task_key: string, group: string) => ({ task_key, group, state: 'running' });
    const grid = sampleGrid({
      state: 'running',
      tasks: [
        task('Z:sample=S1', 'Z'),
        // A wildcard value may hold a `:`; only the first one ends the rule (`split_once`).
        task('A:ref=x:y,sample=S1', 'A'),
        // An untagged Nextflow task whose inner is not an index names its sample (`nextflow_key`).
        task('P (S2)', 'P'),
        task('P (3)', 'P'),
      ],
      dag_nodes: ['Z', 'A', 'P'],
      // An edge from outside the graph never holds its target back (`step_order`).
      dag_edges: [
        ['X', 'Z'],
        ['Z', 'A'],
      ],
    });
    expect(grid.samples.map((s) => s.id)).toEqual(['S1', 'S2']);
    expect(grid.steps.map((s) => [s.group, s.scope])).toEqual([
      ['P', 'sample'],
      ['Z', 'sample'],
      ['A', 'sample'],
    ]);
    expect(grid.unattributed).toBe(1);
  });
});

describe('the mock’s status handlers', () => {
  it('buckets the status history from the request’s time, however long the mock is up', async () => {
    const { STATUS_ROUTES } = await mockModule<{ STATUS_ROUTES: [string, string, Handler][] }>(
      'handlers/status.mjs',
    );
    const history = STATUS_ROUTES.find(([, p]) => p === '/status/history')![2];
    // A day after the mock started, the newest 1h-window bucket is the current five minutes.
    vi.useFakeTimers({ toFake: ['Date'] });
    vi.setSystemTime(Date.now() + 86_400_000);
    let answer: { components: { buckets: { t: number }[] }[] } | undefined;
    history({
      query: new URLSearchParams({ window: '1h' }),
      send: ((_status, body) => (answer = body as typeof answer)) as Send,
    });
    const newest = Math.max(...answer!.components[0].buckets.map((b) => b.t));
    expect(Date.now() / 1000 - newest).toBeLessThan(300);
  });
});

describe('the mock’s output handlers', () => {
  it('names, on every output, a task its run has', async () => {
    const { OUTPUTS } = await mockModule<{
      OUTPUTS: Record<string, { task_key?: string; reused_from?: string }[]>;
    }>('fixtures/outputs.mjs');
    const { RUNS } = await mockModule<{
      RUNS: { run_id: string; tasks?: { task_key: string }[] }[];
    }>('fixtures/runs.mjs');
    let checked = 0;
    for (const [run, outputs] of Object.entries(OUTPUTS)) {
      const tasks = new Set(RUNS.find((r) => r.run_id === run)?.tasks?.map((t) => t.task_key));
      if (tasks.size === 0) continue;
      checked += 1;
      // A reused output names the task of the run it came from.
      for (const { task_key, reused_from } of outputs)
        if (task_key && !reused_from) expect(tasks.has(task_key), `${run}: ${task_key}`).toBe(true);
    }
    // The Lab runs at least: a typo in a run id must not skip them all.
    expect(checked).toBeGreaterThanOrEqual(3);
  });

  it('refuses to serve an output biffix does not serve, as servable_output does', async () => {
    const { OUTPUTS } = await mockModule<{ OUTPUTS: Record<string, Record<string, unknown>[]> }>(
      'fixtures/outputs.mjs',
    );
    const { OUTPUT_ROUTES } = await mockModule<{ OUTPUT_ROUTES: [string, string, Handler][] }>(
      'handlers/outputs.mjs',
    );
    // This module's own copy of the fixtures: an output outside the project bucket.
    const run = 'run_01HGW2wgs30x';
    OUTPUTS[run].push({ ...OUTPUTS[run][0], id: 'elsewhere', servable: false });
    for (const read of ['preview', 'view-url', 'download', 'fastqc', 'alignment-view']) {
      const handler = OUTPUT_ROUTES.find(
        ([, p]) => p === `/runs/{run_id}/outputs/{output_id}/${read}`,
      )![2];
      let answer: [number, unknown] | undefined;
      handler({
        params: { run_id: run, output_id: 'elsewhere' },
        query: new URLSearchParams(),
        send: ((status, body) => (answer = [status, body])) as Send,
      });
      expect(answer?.[0], read).toBe(403);
    }
  });
});

describe('the mock’s run handlers', () => {
  it('measures a live run’s running tasks against the clock that moves the run', async () => {
    const { listTasks, keepLiveRunsCurrent } = await mockModule<{
      listTasks: Handler;
      keepLiveRunsCurrent: () => void;
    }>('handlers/runs.mjs');
    // An hour after the mock started, its live run's times have moved with the wall clock.
    vi.useFakeTimers({ toFake: ['Date'] });
    vi.setSystemTime(Date.now() + 3600_000);
    keepLiveRunsCurrent();
    let page: { tasks: { task_key: string }[] } | undefined;
    const send: Send = (status, body) => {
      expect(status).toBe(200);
      page = body as typeof page;
    };
    listTasks({
      params: { run_id: 'run_01HGW8nflive' },
      query: new URLSearchParams({ sort: 'duration' }),
      send,
    });
    // Longest first: the running ones (minutes in) before the finished minute-long ones.
    expect(page!.tasks.map((t) => t.task_key)).toEqual([
      'REVERSE (1)',
      'REVERSE (2)',
      'REVERSE (3)',
      'SAY (1)',
      'SAY (2)',
    ]);
  });
});
