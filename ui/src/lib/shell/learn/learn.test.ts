import { describe, expect, it } from 'vitest';
import {
  learnFile,
  learnModeFor,
  learnPageFor,
  loadLearn,
  parseLearn,
  type LearnLoader,
  type LearnPage,
} from './learn';

describe('learnPageFor', () => {
  it.each([
    ['/', 'home', 'home'],
    ['/runs', 'runs', 'runs'],
    ['/runs/new', 'wizard', 'new-run'],
    ['/runs/r-1', 'run', 'run'],
    ['/runs/r-1/jobs', 'run', 'run'],
    ['/runs/r-1/outputs', 'run', 'run'],
    ['/workflows', 'workflows', 'workflows'],
    ['/workflows/abc%3Av1', 'wizard', 'wizard'],
    ['/workflows/abc%3Av1/about', 'workflow', 'workflow'],
    ['/workflows/abc%3Av1/provenance', 'workflow', 'workflow'],
    ['/workflows/abc%3Av1/run', 'wizard', 'wizard'],
    ['/workflows/preview', 'wizard', 'wizard'],
    ['/experiments', 'experiments', 'experiments'],
    ['/experiments/e-1', 'experiment', 'experiment'],
    ['/data', 'data', 'data'],
    ['/data/src/a/b', 'data', 'data'],
    ['/settings', 'settings', 'settings'],
    ['/settings/references', 'references', 'references'],
    ['/usage', 'usage', 'usage'],
    ['/status', 'status', 'status'],
    ['/api-docs', 'api', 'api'],
    ['/somewhere-else', 'home', 'home'],
  ] as const)('%s is %s in Lab, %s in Expert', (path, lab, expert) => {
    expect(learnPageFor(path, 'lab')).toBe(lab);
    expect(learnPageFor(path, 'expert')).toBe(expert);
  });
});

describe('every page of the app', () => {
  // Each route, by its SvelteKit id, as a path to it. A new route fails here until it is added,
  // with the Learn page it gets, so none quietly falls back to My work's help.
  const ROUTES: Record<string, { path: string; lab: LearnPage | null; expert: LearnPage | null }> =
    {
      '/': { path: '/', lab: 'home', expert: 'home' },
      '/api-docs': { path: '/api-docs', lab: 'api', expert: 'api' },
      // Chromeless: no Learn button.
      '/auth/callback': { path: '/auth/callback', lab: null, expert: null },
      '/signed-out': { path: '/signed-out', lab: null, expert: null },
      '/data/[...rest]': { path: '/data/core/a', lab: 'data', expert: 'data' },
      '/experiments': { path: '/experiments', lab: 'experiments', expert: 'experiments' },
      '/experiments/[id]': { path: '/experiments/e', lab: 'experiment', expert: 'experiment' },
      '/runs': { path: '/runs', lab: 'runs', expert: 'runs' },
      '/runs/new': { path: '/runs/new', lab: 'wizard', expert: 'new-run' },
      '/runs/[id]': { path: '/runs/r', lab: 'run', expert: 'run' },
      '/runs/[id]/coordinator': { path: '/runs/r/coordinator', lab: 'run', expert: 'run' },
      '/runs/[id]/cost': { path: '/runs/r/cost', lab: 'run', expert: 'run' },
      '/runs/[id]/files': { path: '/runs/r/files', lab: 'run', expert: 'run' },
      '/runs/[id]/graph': { path: '/runs/r/graph', lab: 'run', expert: 'run' },
      '/runs/[id]/jobs': { path: '/runs/r/jobs', lab: 'run', expert: 'run' },
      '/runs/[id]/outputs': { path: '/runs/r/outputs', lab: 'run', expert: 'run' },
      '/runs/[id]/params': { path: '/runs/r/params', lab: 'run', expert: 'run' },
      '/runs/[id]/reports': { path: '/runs/r/reports', lab: 'run', expert: 'run' },
      '/settings': { path: '/settings', lab: 'settings', expert: 'settings' },
      '/settings/references': {
        path: '/settings/references',
        lab: 'references',
        expert: 'references',
      },
      '/status': { path: '/status', lab: 'status', expert: 'status' },
      '/usage': { path: '/usage', lab: 'usage', expert: 'usage' },
      '/workflows': { path: '/workflows', lab: 'workflows', expert: 'workflows' },
      '/workflows/preview': { path: '/workflows/preview', lab: 'wizard', expert: 'wizard' },
      '/workflows/[id]': { path: '/workflows/x', lab: 'wizard', expert: 'wizard' },
      '/workflows/[id]/about': { path: '/workflows/x/about', lab: 'workflow', expert: 'workflow' },
      '/workflows/[id]/parameters': {
        path: '/workflows/x/parameters',
        lab: 'workflow',
        expert: 'workflow',
      },
      '/workflows/[id]/provenance': {
        path: '/workflows/x/provenance',
        lab: 'workflow',
        expert: 'workflow',
      },
      '/workflows/[id]/run': { path: '/workflows/x/run', lab: 'wizard', expert: 'wizard' },
      '/workflows/[id]/versions': {
        path: '/workflows/x/versions',
        lab: 'workflow',
        expert: 'workflow',
      },
    };
  const routeIds = Object.keys(import.meta.glob('/src/routes/**/+page.svelte'))
    .map((f) => f.replace(/^\/src\/routes/, '').replace(/\/\+page\.svelte$/, '') || '/')
    .sort();

  it('is each listed, with the Learn page it gets', () => {
    expect(routeIds).toEqual(Object.keys(ROUTES).sort());
    for (const [id, { path, lab, expert }] of Object.entries(ROUTES)) {
      if (lab) expect(learnPageFor(path, 'lab'), `${id} lab`).toBe(lab);
      if (expert) expect(learnPageFor(path, 'expert'), `${id} expert`).toBe(expert);
    }
  });

  it('has a Learn file in the words of each view it is shown in', () => {
    const files = Object.keys(import.meta.glob('./*.md'));
    for (const [id, { path, lab, expert }] of Object.entries(ROUTES)) {
      for (const [mode, page] of [
        ['lab', lab],
        ['expert', expert],
      ] as const) {
        if (!page) continue;
        const file = `./${page}.${learnModeFor(path, mode)}.md`;
        expect(files, `${id} ${mode}`).toContain(file);
      }
    }
  });
});

describe('the Learn files', () => {
  it('are exactly both views of each page, but Expert only for the Expert-only pages', () => {
    const files = Object.keys(import.meta.glob('./*.md')).sort();
    const both: LearnPage[] = [
      'home',
      'runs',
      'run',
      'workflows',
      'workflow',
      'wizard',
      'experiments',
      'experiment',
      'data',
      'settings',
    ];
    const expertOnly: LearnPage[] = ['new-run', 'references', 'usage', 'status', 'api'];
    const expected = [
      ...both.flatMap((p) => [`./${p}.lab.md`, `./${p}.expert.md`]),
      ...expertOnly.map((p) => `./${p}.expert.md`),
    ].sort();
    expect(files).toEqual(expected);
  });
});

describe('learnModeFor', () => {
  it('explains an Expert-only page in Expert words, even in the Lab view', () => {
    expect(learnModeFor('/runs/r-1/jobs', 'lab')).toBe('expert');
    expect(learnModeFor('/workflows/x/provenance', 'lab')).toBe('expert');
    expect(learnModeFor('/usage', 'lab')).toBe('expert');
  });

  it('keeps the view on show everywhere else', () => {
    expect(learnModeFor('/runs/r-1', 'lab')).toBe('lab');
    expect(learnModeFor('/workflows/x/about', 'lab')).toBe('lab');
    expect(learnModeFor('/runs/r-1', 'expert')).toBe('expert');
  });
});

describe('parseLearn', () => {
  it('splits a title, a pitch and the how-to cards', () => {
    const c = parseLearn(
      '# About it\nThe pitch,\non two lines.\n\n## How?\nLike *this*.\n\n## Why?\nBecause.\n',
    );
    expect(c.title).toBe('About it');
    expect(c.pitch).toBe('The pitch,\non two lines.');
    expect(c.cards).toEqual([
      { title: 'How?', body: 'Like *this*.' },
      { title: 'Why?', body: 'Because.' },
    ]);
  });

  it('leaves a heading-like line in a code block as text', () => {
    const c = parseLearn('# T\nPitch.\n\n## How?\n```\n## not a card\n```\n');
    expect(c.cards).toEqual([{ title: 'How?', body: '```\n## not a card\n```' }]);
  });
});

describe('learnFile', () => {
  const files = { './a.lab.md': 'lab', './a.expert.md': 'expert', './b.expert.md': 'only' };

  it("picks the view's own file, else the other view's", () => {
    expect(learnFile('a' as LearnPage, 'lab', files)).toBe('lab');
    expect(learnFile('a' as LearnPage, 'expert', files)).toBe('expert');
    expect(learnFile('b' as LearnPage, 'lab', files)).toBe('only');
    expect(learnFile('c' as LearnPage, 'lab', files)).toBeNull();
  });
});

describe('the Learn content', () => {
  const PATHS = [
    '/',
    '/runs',
    '/runs/new',
    '/runs/r-1',
    '/runs/r-1/jobs',
    '/workflows',
    '/workflows/x/about',
    '/workflows/x/run',
    '/workflows/preview',
    '/experiments',
    '/experiments/e',
    '/data',
    '/settings',
    '/settings/references',
    '/usage',
    '/status',
    '/api-docs',
  ];

  it('gives every page, in both views, a title, a pitch and three how-tos', async () => {
    for (const path of PATHS) {
      for (const mode of ['lab', 'expert'] as const) {
        const c = await loadLearn(path, mode);
        expect(c, `${path} ${mode}`).not.toBeNull();
        expect(c!.title, `${path} ${mode}`).not.toBe('');
        expect(c!.pitch, `${path} ${mode}`).not.toBe('');
        expect(c!.cards, `${path} ${mode}`).toHaveLength(3);
      }
    }
  });

  it('speaks each view in its own words', async () => {
    expect((await loadLearn('/runs', 'lab'))!.title).toBe('About Analyses');
    expect((await loadLearn('/runs', 'expert'))!.title).toBe('About Runs');
    expect((await loadLearn('/workflows', 'lab'))!.title).toBe('About pipelines');
    expect((await loadLearn('/workflows', 'expert'))!.title).toBe('About Workflows');
    expect((await loadLearn('/runs/new', 'expert'))!.title).toBe('About New run');
    expect((await loadLearn('/runs/r-1/jobs', 'lab'))!.title).toBe('About this run');
  });

  it('loads only the file it shows', async () => {
    const asked: string[] = [];
    const loader =
      (name: string): LearnLoader =>
      () => {
        asked.push(name);
        return Promise.resolve(`# ${name}\nPitch.`);
      };
    const files = {
      './runs.lab.md': loader('runs.lab'),
      './runs.expert.md': loader('runs.expert'),
    };
    expect((await loadLearn('/runs', 'lab', files))!.title).toBe('runs.lab');
    expect(asked).toEqual(['runs.lab']);
    expect(await loadLearn('/usage', 'lab', files)).toBeNull();
  });
});
