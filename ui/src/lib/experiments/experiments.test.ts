import { describe, expect, it } from 'vitest';
import type { RunSummary } from '$lib/api';
import {
  activityText,
  anyLive,
  attentionText,
  cappedRunsText,
  experimentProjects,
  failedCount,
  mayHaveResults,
  nameProblem,
  NAME_INPUT_MAXLENGTH,
  runCountText,
  samplesCsvAbout,
  samplesCsvFilename,
  samplesCsvUnavailable,
  samplesSummary,
  sampleTable,
  totalCostText,
  withExperiment,
} from './experiments';

const exp = (over: Record<string, unknown> = {}) => ({
  id: 'e1',
  name: 'Liver tumor study',
  project: 'genomics',
  archived_at: null,
  ...over,
});

describe('nameProblem', () => {
  const existing = [
    exp(),
    exp({ id: 'e2', name: 'Pilot', archived_at: 100 }),
    exp({ id: 'e3', name: 'Other', project: 'imported' }),
  ];
  it('takes a fresh name, trimmed', () => {
    expect(nameProblem('  Kidney study ', 'genomics', existing)).toBeNull();
  });
  it('refuses a blank name and one over 200 characters', () => {
    expect(nameProblem('   ', 'genomics', existing)).toMatch(/Give the experiment a name/);
    expect(nameProblem('x'.repeat(200), 'genomics', [])).toBeNull();
    expect(nameProblem(` ${'x'.repeat(201)} `, 'genomics', [])).toMatch(/200 characters.*201/);
  });
  it('counts characters as the server does, an emoji as one', () => {
    // '🧬' is one code point but two UTF-16 units.
    expect(nameProblem('🧬'.repeat(200), 'genomics', [])).toBeNull();
    expect(nameProblem('🧬'.repeat(201), 'genomics', [])).toMatch(/200 characters.*201/);
    expect('🧬'.repeat(200).length).toBeLessThanOrEqual(NAME_INPUT_MAXLENGTH);
  });
  it("refuses an open experiment's name in the same project, ignoring case and spaces", () => {
    expect(nameProblem(' liver TUMOR study', 'genomics', existing)).toBe(
      '“Liver tumor study” is already an experiment in project genomics.',
    );
  });
  it("frees an archived one's name, another project's, and its own on a rename", () => {
    expect(nameProblem('pilot', 'genomics', existing)).toBeNull();
    expect(nameProblem('Other', 'genomics', existing)).toBeNull();
    expect(nameProblem('Liver tumor study', 'genomics', existing, 'e1')).toBeNull();
  });
});

describe('sampleTable', () => {
  const run = (run_id: string, run_name: string | null) => ({ run_id, run_name }) as never;
  it('puts the analyses oldest first and each sample’s state under them', () => {
    const t = sampleTable({
      // Newest first, as the server lists them.
      runs: [run('r3', 'batch-12'), run('r2', null), run('r1', ' batch-10 ')],
      samples: [
        { id: 'Tumor_1', by_run: { r1: 'done', r3: 'running' } },
        { id: 'Tumor_4', by_run: { r1: 'done', r2: 'failed' } },
      ],
    });
    expect(t.columns).toEqual([
      { runId: 'r1', label: 'batch-10' },
      { runId: 'r2', label: 'r2' },
      { runId: 'r3', label: 'batch-12' },
    ]);
    expect(t.rows).toEqual([
      { id: 'Tumor_1', cells: ['done', null, 'running'] },
      { id: 'Tumor_4', cells: ['done', 'failed', null] },
    ]);
  });
  it('gives no column to an analysis without samples', () => {
    const t = sampleTable({
      runs: [run('r2', 'variants'), run('r1', 'batch-10')],
      samples: [{ id: 'S1', by_run: { r1: 'done' } }],
    });
    expect(t.columns.map((c) => c.runId)).toEqual(['r1']);
    expect(sampleTable({ runs: [run('r1', 'x')], samples: [] })).toEqual({ columns: [], rows: [] });
  });
  it('tells apart analyses whose names are alike', () => {
    // Two unnamed analyses of one pipeline started the same minute on the same samples.
    const unnamed = (run_id: string) =>
      ({
        run_id,
        workflow_name: 'nf-core/sarek',
        submitted_at: new Date(2026, 9, 1, 14, 5).getTime() / 1000,
        samples: { total: 3 },
      }) as never;
    const t = sampleTable({
      runs: [unnamed('run_01HGWbbbbbb'), unnamed('run_01HGWaaaaaa'), run('r1', 'solo')],
      samples: [
        { id: 'S1', by_run: { run_01HGWaaaaaa: 'done', run_01HGWbbbbbb: 'done', r1: 'done' } },
      ],
    });
    const labels = t.columns.map((c) => c.label);
    expect(new Set(labels).size).toBe(3);
    expect(labels[0]).toBe('solo');
    expect(labels[1]).toMatch(/^nf-core\/sarek · Oct 1(, \d{4})? 14:05 · 3 samples \(…aaaaaa\)$/);
    expect(labels[2]).toMatch(/\(…bbbbbb\)$/);
  });
});

describe('totals and words', () => {
  it('says the total is an estimate while an analysis is running', () => {
    expect(totalCostText(41.2, false)).toBe('$41.20');
    expect(totalCostText(41.2, true)).toBe('$41.20 · estimate');
    expect(totalCostText(null, true)).toBeNull();
    expect(totalCostText(undefined, false)).toBeNull();
    expect(totalCostText(0, false)).toBe('$0.00');
  });
  it('tells a running analysis from ended ones, and counts the failed', () => {
    expect(anyLive([{ terminal: true }, { terminal: false }])).toBe(true);
    expect(anyLive([{ terminal: true }, {}])).toBe(false);
    expect(failedCount([{ state: 'failed' }, { state: 'succeeded' }, { state: 'failed' }])).toBe(2);
  });
  it('counts in each view’s words', () => {
    expect(runCountText(1, 'lab')).toBe('1 analysis');
    expect(runCountText(3, 'lab')).toBe('3 analyses');
    expect(runCountText(3, 'expert')).toBe('3 runs');
    expect(samplesSummary(24, 3, 'lab')).toBe('24 samples across 3 analyses');
    expect(samplesSummary(1, 1, 'expert')).toBe('1 sample across 1 run');
  });
  it('says when one last started, else when it was made', () => {
    const now = 1_000_000;
    expect(activityText({ run_count: 2, last_run_at: now - 7200, created_at: 0 }, 'lab', now)).toBe(
      '2 analyses · last started 2 h ago',
    );
    expect(activityText({ run_count: 0, created_at: now - 86400 }, 'expert', now)).toBe(
      '0 runs · created yesterday',
    );
  });
  it('says how many of the caller’s analyses need a look, and nothing when none does', () => {
    expect(attentionText({ needs_attention: 1 })).toBe('1 needs a look');
    expect(attentionText({ needs_attention: 3 })).toBe('3 need a look');
    expect(attentionText({ needs_attention: 0 })).toBe('');
  });
  it('says when the page holds only the newest of its runs', () => {
    expect(cappedRunsText(200, 250, 'lab')).toBe('Showing the 200 newest of 250 analyses.');
    expect(cappedRunsText(200, 201, 'expert')).toBe('Showing the 200 newest of 201 runs.');
  });
  it('offers the pipelines’ and experiments’ projects, each once', () => {
    expect(
      experimentProjects(['genomics', 'imported', 'genomics'], [{ project: 'z-lab' }]),
    ).toEqual(['genomics', 'imported', 'z-lab']);
  });
  it('knows which analyses can have results yet', () => {
    expect(mayHaveResults({ terminal: false, outputs_total: 0 })).toBe(false);
    expect(mayHaveResults({ terminal: false, outputs_total: 3 })).toBe(true);
    expect(mayHaveResults({ terminal: true })).toBe(true);
    // A running resume shows the results it reused before it registers any of its own.
    expect(mayHaveResults({ terminal: false, outputs_total: 0, resumed: true })).toBe(true);
    expect(mayHaveResults({ terminal: false, outputs_total: 0, resumed: false })).toBe(false);
  });
  it('carries the experiment into a launch link', () => {
    expect(withExperiment('/runs/new', 'exp 1')).toBe('/runs/new?experiment=exp+1');
    expect(withExperiment('/workflows/a%3Av1/run', 'e1')).toBe(
      '/workflows/a%3Av1/run?experiment=e1',
    );
    expect(withExperiment('/x?step=2', 'e1')).toBe('/x?step=2&experiment=e1');
    expect(withExperiment('/runs/new', null)).toBe('/runs/new');
    // A fragment stays at the end, and a link already naming an experiment names this one.
    expect(withExperiment('/x?step=2#review', 'e1')).toBe('/x?step=2&experiment=e1#review');
    expect(withExperiment('/x?experiment=old', 'e1')).toBe('/x?experiment=e1');
  });
});

describe('the samples CSV', () => {
  it('is named after the experiment, safe as a file name, as the server names it', () => {
    // The same cases as `the_file_name_follows_the_ui_s_rule` in crates/biffix-backend/src/runs/samples_csv.rs.
    for (const [name, want] of [
      ['Liver tumor study', 'Liver tumor study-samples.csv'],
      ['Café 鼠 · batch 2', 'Café 鼠 · batch 2-samples.csv'],
      ['a/b\\c:d*e?f"g<h>i|j\tk', 'a-b-c-d-e-f-g-h-i-j-k-samples.csv'],
      ['Liver: batch 1', 'Liver- batch 1-samples.csv'],
      ['  ', 'experiment-samples.csv'],
      ['', 'experiment-samples.csv'],
      [' \ufeffx\u0085 ', 'x--samples.csv'],
    ])
      expect(samplesCsvFilename(name), JSON.stringify(name)).toBe(want);
  });
  it('says what it holds', () => {
    expect(samplesCsvAbout('lab')).toBe(
      'One row per sample per analysis, for every analysis in it; analyses that name no samples yet are left out.',
    );
    expect(samplesCsvAbout('expert')).toContain('per run, for every run');
  });
  it('says why it cannot be downloaded when there are no samples', () => {
    const run = { run_id: 'r1' } as RunSummary;
    const sample = { id: 'S1', by_run: { r1: 'done' } };
    expect(samplesCsvUnavailable({ runs: [run], runs_total: 1, samples: [sample] }, 'lab')).toBe(
      null,
    );
    expect(samplesCsvUnavailable({ runs: [], runs_total: 0, samples: [] }, 'lab')).toBe(
      'No samples yet: there are no analyses in it.',
    );
    expect(samplesCsvUnavailable({ runs: [run], runs_total: 1, samples: [] }, 'expert')).toBe(
      'No samples yet: none of its runs has named a sample.',
    );
    // Cut short: an older run the detail left out may have samples.
    expect(samplesCsvUnavailable({ runs: [run], runs_total: 300, samples: [] }, 'lab')).toBe(null);
  });
});
