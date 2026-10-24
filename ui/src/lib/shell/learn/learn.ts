// The Learn panel's content: one Markdown file per page and view in this
// folder (`<page>.lab.md`, `<page>.expert.md`), each a title, a one-paragraph pitch and three
// how-to cards. Which page a path is, and how a file splits up, are pure functions over strings;
// the files themselves load only when the panel asks for one.

import { lexer } from 'marked';
import { isExpertOnly, type ViewMode } from '#lib/view/viewRoutes.js';

/** Where the Learn panel's "Read the guide" link goes: the user guide, in the public repository. */
export const GUIDE_URL = 'https://github.com/nh13/biffix/blob/main/docs/user-guide.md';

/** The pages the Learn panel knows, one content file (per view) each. */
export type LearnPage =
  | 'home'
  | 'runs'
  | 'run'
  | 'new-run'
  | 'workflows'
  | 'workflow'
  | 'wizard'
  | 'experiments'
  | 'experiment'
  | 'data'
  | 'settings'
  | 'references'
  | 'usage'
  | 'status'
  | 'api';

/**
 * The page `path` is in `mode`, for its Learn content (a page with none of its own gets My work's).
 * `/runs/new` is two pages: step 1 of the Lab's launch, and the Expert view's workflow chooser.
 */
export function learnPageFor(path: string, mode: ViewMode): LearnPage {
  const parts = path.split('/').filter(Boolean);
  const [top, id, tab] = parts;
  switch (top) {
    case undefined:
      return 'home';
    case 'runs':
      if (id === 'new') return mode === 'lab' ? 'wizard' : 'new-run';
      return id ? 'run' : 'runs';
    case 'workflows':
      if (!id) return 'workflows';
      // An unregistered workflow's launch form, as the Run tab's.
      if (id === 'preview') return 'wizard';
      return tab === 'run' || tab === undefined ? 'wizard' : 'workflow';
    case 'experiments':
      return id ? 'experiment' : 'experiments';
    case 'data':
      return 'data';
    case 'settings':
      return id === 'references' ? 'references' : 'settings';
    case 'usage':
      return 'usage';
    case 'status':
      return 'status';
    case 'api-docs':
      return 'api';
    default:
      return 'home';
  }
}

export interface LearnCard {
  title: string;
  /** Markdown. */
  body: string;
}

export interface LearnContent {
  title: string;
  /** Markdown. */
  pitch: string;
  cards: LearnCard[];
}

/**
 * Split a Learn file: its `# ` heading is the title, the text before the first `## ` the pitch, and
 * each `## ` heading with the text under it a how-to card. Headings are read as Markdown reads
 * them, so a `#` line inside a code block stays text.
 */
export function parseLearn(markdown: string): LearnContent {
  let title = '';
  const pitch: string[] = [];
  const cards: { title: string; body: string[] }[] = [];
  for (const token of lexer(markdown)) {
    if (token.type === 'heading' && token.depth === 1 && !title) title = token.text;
    else if (token.type === 'heading' && token.depth === 2)
      cards.push({ title: token.text, body: [] });
    else (cards.at(-1)?.body ?? pitch).push(token.raw);
  }
  return {
    title,
    pitch: pitch.join('').trim(),
    cards: cards.map((c) => ({ title: c.title, body: c.body.join('').trim() })),
  };
}

/** Loads one Learn file's text. */
export type LearnLoader = () => Promise<string>;

/** Every Learn file in this folder, by file name, each loaded on first use. */
const FILES: Record<string, LearnLoader> = import.meta.glob<string>('./*.md', {
  query: '?raw',
  import: 'default',
});

/**
 * The view whose words a Learn file for `path` is in: the view on show, except that an Expert-only
 * page (one a Lab user reaches only by a link) is explained in the Expert view's words, as it is
 * drawn in them.
 */
export function learnModeFor(path: string, mode: ViewMode): ViewMode {
  return mode === 'lab' && isExpertOnly(path) ? 'expert' : mode;
}

/** The Learn file for `page` in `mode`, else the other view's (a page only one view has). */
export function learnFile<T>(page: LearnPage, mode: ViewMode, files: Record<string, T>): T | null {
  const other: ViewMode = mode === 'lab' ? 'expert' : 'lab';
  return files[`./${page}.${mode}.md`] ?? files[`./${page}.${other}.md`] ?? null;
}

/** The Learn panel's content for `path` in `mode` (`null` for a page with no file). */
export async function loadLearn(
  path: string,
  mode: ViewMode,
  files: Record<string, LearnLoader> = FILES,
): Promise<LearnContent | null> {
  const load = learnFile(learnPageFor(path, mode), learnModeFor(path, mode), files);
  return load ? parseLearn(await load()) : null;
}
