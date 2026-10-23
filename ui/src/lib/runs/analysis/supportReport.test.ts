import { describe, expect, it } from 'vitest';
import { askHref, mailtoAddress, supportReport, type ReportRun } from './supportReport';
import type { FailureItem } from './failure';

const run: ReportRun = {
  run_id: 'run_01HGWEoom',
  run_name: 'rnaseq · batch-11',
  workflow: 'c4c4c4c4c4c4:v1',
  workflow_name: 'lab-rnaseq',
  submitter: 'user:priya',
  submitter_label: 'priya@lab.example.org',
  submitted_at: 1_790_000_000,
  completed_at: 1_790_003_600,
  state: 'failed',
  params: {
    aligner: 'star_salmon',
    api_token: 'sk-live-TOPSECRET',
    password: '[REDACTED]',
    extra: { depth: 2 },
  },
  input_uris: ['s3://deliveries/batch-11/'],
  reference_bundle: { id: 'grcm39', uris: {} },
  samples: { total: 4, done: 3, failed: 1, declared: true },
};

const oom: FailureItem = {
  sample: 'Tumor_4',
  step: 'STAR_ALIGN',
  task_key: 'STAR_ALIGN (Tumor_4) #7',
  cause: 'out_of_memory',
  message: 'it ran out of memory',
  advice: 'Ask for more memory.',
  attempts: 2,
  exit_code: 137,
  batch_job_id: '5f0c6a2e-7b1d-4c9e-9a51-2f3e8d4b6c01',
  reason: 'OutOfMemoryError: Container killed due to memory usage',
  log_tail: Array.from({ length: 30 }, (_, i) => `line ${i + 1}`),
  try_again: 'resume',
};

const report = (over: Partial<Parameters<typeof supportReport>[0]> = {}) =>
  supportReport({
    run,
    url: 'https://biffix.example/runs/run_01HGWEoom',
    items: [oom],
    fields: [{ name: 'api_token', is_secret: true }],
    ...over,
  });

describe('supportReport', () => {
  it('never carries a secret value, whether the version marks it or the server redacted it', () => {
    const text = report();
    expect(text).not.toContain('sk-live-TOPSECRET');
    expect(text).not.toContain('[REDACTED]');
    expect(text).toContain('  api_token: [secret]');
    expect(text).toContain('  password: [secret]');
    expect(text).toContain('  aligner: star_salmon');
    expect(text).toContain('  extra: {"depth":2}');
  });

  it('names the analysis, where it is, and its pipeline, submitter, times and state', () => {
    const text = report();
    for (const line of [
      'Analysis: rnaseq · batch-11',
      'Link: https://biffix.example/runs/run_01HGWEoom',
      'Run id: run_01HGWEoom',
      'Pipeline: lab-rnaseq (version c4c4c4c4c4c4:v1)',
      'Started by: priya@lab.example.org',
      'Submitted: 2026-09-21T14:13:20Z',
      'State: failed',
      'Samples: 4 total, 3 done, 1 failed',
      'Reference bundle: grcm39',
      '  s3://deliveries/batch-11/',
    ])
      expect(text).toContain(line);
  });

  it('names an unnamed analysis as the page does, never by its run id alone', () => {
    const text = report({
      run: { ...run, run_name: null, workflow_title: 'Bulk RNA-seq', submitted_at: undefined },
    });
    expect(text).toContain('Analysis: Bulk RNA-seq · 4 samples');
    expect(text).toContain('Run id: run_01HGWEoom');
  });

  it('describes each failure, with only the last 20 log lines', () => {
    const text = report();
    expect(text).toContain('Failure 1: Tumor_4 · STAR align · it ran out of memory');
    expect(text).toContain('  Step: STAR_ALIGN');
    expect(text).toContain('  Task: STAR_ALIGN (Tumor_4) #7');
    expect(text).toContain('  Cause: out_of_memory');
    expect(text).toContain('  Tries: 2');
    expect(text).toContain('  Exit code: 137');
    expect(text).toContain('  Batch job: 5f0c6a2e-7b1d-4c9e-9a51-2f3e8d4b6c01');
    expect(text).toContain('  Reason: OutOfMemoryError');
    expect(text).toContain('  Last 20 log lines:');
    expect(text).toContain('    line 30');
    expect(text).not.toContain('    line 10\n');
    expect(text.length).toBeLessThan(2000);
  });

  it('reads well for a cancelled run with no failures and no params', () => {
    const text = report({
      run: { ...run, state: 'cancelled', params: {}, completed_at: null, input_uris: [] },
      items: [],
    });
    expect(text).toContain('Finished: not yet');
    expect(text).toContain('Params: none');
    expect(text).toContain('Inputs: none');
    expect(text).not.toContain('Failure 1');
  });

  it('says when the failures could not be read, rather than looking like there were none', () => {
    const text = report({ run, items: [], failuresUnread: 'Bad gateway' });
    expect(text).toContain('\nFailures: could not be read (Bad gateway)\n');
    expect(report({ run, items: [] })).not.toContain('Failures:');
  });
});

describe('askHref', () => {
  it('writes to the contact about the analysis, with the diagnosis and where the report goes', () => {
    const href = askHref(
      { name: 'Sam Okafor', email: 'sam@lab.example.org' },
      'batch-11',
      'https://biffix.example/runs/r',
      'Tumor_4 · STAR_ALIGN · it ran out of memory',
    );
    const url = new URL(href);
    expect(url.protocol).toBe('mailto:');
    expect(url.pathname).toBe('sam@lab.example.org');
    expect(url.searchParams.get('subject')).toBe('biffix: batch-11 needs attention');
    const body = url.searchParams.get('body')!;
    expect(body).toContain('https://biffix.example/runs/r');
    expect(body).toContain('Tumor_4 · STAR_ALIGN · it ran out of memory');
    expect(body).toContain('Paste the copied report here.');
  });
});

describe('mailtoAddress', () => {
  it('keeps an address one recipient, whatever it holds', () => {
    expect(mailtoAddress(' core@lab.example.org ')).toBe('core@lab.example.org');
    for (const email of [
      'a?bcc=x@lab.example.org',
      'core@lab.example.org#x',
      'a&b@lab.example.org',
      'a,b@lab.example.org',
    ]) {
      const href = askHref({ name: 'Core', email }, 'batch-11', 'https://b/runs/r', null);
      const url = new URL(href);
      // The address is the whole path: no header, recipient or fragment leaks out of it.
      expect(decodeURIComponent(url.pathname)).toBe(email);
      expect(url.hash).toBe('');
      expect([...url.searchParams.keys()]).toEqual(['subject', 'body']);
      expect(url.searchParams.get('subject')).toBe('biffix: batch-11 needs attention');
    }
    expect(mailtoAddress('a?bcc=x@lab.example.org')).toBe('a%3Fbcc%3Dx@lab.example.org');
  });
});
