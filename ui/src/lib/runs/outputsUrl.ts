// The Outputs view's state in the URL (`/runs/<id>/outputs?view=flat&work=1#q=…&task=…&file=<id>`),
// so a filtered listing or an open file can be linked. The search text, the task and the open file
// can name a sample or a file, so they live in the fragment, which never reaches a server; the
// layout choices stay in the query. Defaults are left out.

import { validParam, withViewParams } from '#lib/util/urlParam.js';

export interface OutputsView {
  view: 'tree' | 'flat';
  q: string;
  /** Include work-directory intermediates. */
  work: boolean;
  /** Only this task's outputs. */
  task: string | null;
  /** The output open in the viewer, by id. */
  file: string | null;
}

/** The view an Outputs tab opens on; a key at its default is left out of the URL. */
export const DEFAULT_OUTPUTS_VIEW: OutputsView = {
  view: 'tree',
  q: '',
  work: false,
  task: null,
  file: null,
};

/** The view keys that can name a sample, a task or a file: they go in the URL fragment. */
export const OUTPUTS_FRAGMENT_KEYS: readonly string[] = ['q', 'task', 'file'];

/** The view `params` ask for (read them with `viewParams`, so the fragment is included). */
export function parseOutputsView(params: URLSearchParams): OutputsView {
  const d = DEFAULT_OUTPUTS_VIEW;
  return {
    view: validParam(params.get('view'), ['tree', 'flat'], d.view),
    q: params.get('q') ?? d.q,
    work: params.has('work') ? params.get('work') === '1' : d.work,
    task: params.get('task') || d.task,
    file: params.get('file') || d.file,
  };
}

/** `view` as params, defaults left out, onto a copy of `base` (other params are kept). */
export function outputsViewParams(view: OutputsView, base?: URLSearchParams): URLSearchParams {
  const params = new URLSearchParams(base);
  const set = (key: string, value: string | null) => {
    if (value) params.set(key, value);
    else params.delete(key);
  };
  const d = DEFAULT_OUTPUTS_VIEW;
  set('view', view.view === d.view ? null : view.view);
  set('q', view.q.trim() || null);
  set('work', view.work === d.work ? null : view.work ? '1' : '0');
  set('task', view.task);
  set('file', view.file);
  return params;
}

/**
 * The link to the Outputs tab at `path` showing `view`: the identifying keys
 * ({@link OUTPUTS_FRAGMENT_KEYS}) in the fragment, the rest (and `base`'s other params) in the
 * query.
 */
export function outputsViewHref(path: string, view: OutputsView, base?: URLSearchParams): string {
  const query = new URLSearchParams();
  const fragment = new URLSearchParams();
  for (const [key, value] of outputsViewParams(view, base))
    (OUTPUTS_FRAGMENT_KEYS.includes(key) ? fragment : query).set(key, value);
  const search = query.toString();
  return withViewParams(search ? `${path}?${search}` : path, fragment);
}
