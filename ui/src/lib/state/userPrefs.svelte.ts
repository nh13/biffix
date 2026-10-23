// The user's preferences (`/me/prefs`): the Lab / Expert view, the pinned pipelines and whether the
// first-visit tour is done. Stored on the server so they follow the user across browsers; the view
// is also cached in this browser so the first paint is already right. Lab is the default. A view is
// presentation only: it never changes what a user may do.
//
// For each of them the latest choice wins: a switch made while the server's copy is still loading is not undone by
// it, and a switch the server did not take is kept here, marked unsynced, and sent again on the next
// load rather than replaced by the server's older value.

import { untrack } from 'svelte';
import { getUserPrefs, putUserPrefs, type UserPrefs } from '#lib/api/index.js';
import { readPref, writePref } from '#lib/util/storage.js';
import type { ViewMode } from '#lib/view/viewRoutes.js';
import { errorMessage } from '#lib/util/format.js';

export type { ViewMode } from '#lib/view/viewRoutes.js';

const VIEW_KEY = 'biffix-view';
/** Set while this browser holds a view the server has not stored yet. */
const UNSYNCED_KEY = 'biffix-view-unsynced';

const isMode = (v: unknown): v is ViewMode => v === 'lab' || v === 'expert';

function cached(): ViewMode | null {
  const v = readPref(VIEW_KEY);
  return isMode(v) ? v : null;
}

/**
 * The current view (Lab until the user or their stored preference says otherwise), and why the last
 * save did not reach the server (`null` once one did).
 */
export const view = $state<{
  mode: ViewMode;
  saveError: string | null;
  pinned: string[];
  pinsLoaded: boolean;
  pinError: string | null;
  tourDone: boolean | null;
}>({
  mode: cached() ?? 'lab',
  saveError: null,
  // Pinned pipelines, by `workflowKey` (project + name); empty until the preferences load, and not
  // offered for change until then (`pinsLoaded`), so a pin never replaces pins it has not seen.
  pinned: [],
  pinsLoaded: false,
  // Why the last pin or unpin was refused (`null` once one succeeds).
  pinError: null,
  // Whether the user finished or skipped the first-visit tour: `null` until the preferences load
  // (the tour never shows on a guess), then the server's `tour_done_at`, then this page's choice.
  tourDone: null,
});

/** Whether the user switched view since this page loaded (their choice then outranks the server's). */
let switched = false;

/** How many view saves this page asked for, and the chain they go out on, one at a time. */
let viewWrites = 0;
let viewSaved: Promise<void> = Promise.resolve();

/**
 * Send `mode` to the server; on failure keep it here, marked unsynced, and say why. It is marked
 * unsynced before it is sent, so a page left before the reply (a reload, a typed URL) sends it again
 * rather than adopting the server's older view. Saves go one at a time and a save a newer switch
 * overtook is never sent, so the server ends with the latest choice; only the latest save's reply
 * says whether it was stored (and clears the unsynced mark).
 */
function save(mode: ViewMode): Promise<void> {
  writePref(UNSYNCED_KEY, 'true');
  const mine = ++viewWrites;
  viewSaved = viewSaved.then(async () => {
    if (mine !== viewWrites) return;
    try {
      await putUserPrefs({ view: mode });
      if (mine !== viewWrites) return;
      if (cached() === mode) writePref(UNSYNCED_KEY, '');
      view.saveError = null;
    } catch (e) {
      if (mine === viewWrites) view.saveError = errorMessage(e);
    }
  });
  return viewSaved;
}

/**
 * Reconcile with the server's stored view: adopt it, unless this browser holds a newer choice (one
 * made on this page, or one an earlier save failed to store or never heard back about), which is
 * sent instead.
 */
export async function loadView(): Promise<void> {
  const unsynced = readPref(UNSYNCED_KEY) === 'true';
  try {
    const prefs = await getUserPrefs();
    adopt(prefs);
    if (unsynced) {
      const mine = cached();
      if (mine) await save(mine);
    } else if (!switched && isMode(prefs.view)) {
      view.mode = prefs.view;
      writePref(VIEW_KEY, prefs.view);
    }
  } catch {
    // The preference is optional: the cached view stands, and an unsynced one is still sent.
    const mine = cached();
    if (unsynced && mine) await save(mine);
  }
}

/** Switch view: applies at once, then records it for this user. */
export async function setView(mode: ViewMode): Promise<void> {
  switched = true;
  view.mode = mode;
  writePref(VIEW_KEY, mode);
  await save(mode);
}

/** How many pin writes this page sent: only the newest one's reply is applied. */
let pinWrites = 0;
/** How many tour writes this page asked for: only the newest one is sent once the last settles. */
let tourWrites = 0;

/**
 * Take the server's pins and tour answer, unless this page already changed them (a pin, or a
 * finish or replay of the tour, holds the newer answer).
 */
function adopt(prefs: UserPrefs): void {
  if (pinWrites === 0) view.pinned = prefs.pinned_workflows ?? [];
  view.pinsLoaded = true;
  if (tourWrites === 0) view.tourDone = prefs.tour_done_at != null;
}

/**
 * Load the saved pins (and the tour's answer) if they have not loaded yet: the layout's first load
 * may have failed. Read untracked, so an effect that calls it never runs again when they arrive.
 */
export async function loadPins(): Promise<void> {
  if (untrack(() => view.pinsLoaded)) return;
  try {
    adopt(await getUserPrefs());
  } catch {
    /* stars stay unavailable until a later load */
  }
}

/**
 * Pin or unpin a pipeline (by `workflowKey`). The server applies it as one operation on the stored
 * list, so pins made elsewhere, or in quick succession, are never lost. Shown at once; the newest
 * reply sets the list, and a refusal is undone (unless a newer change followed) and reported.
 */
export async function togglePin(key: string): Promise<void> {
  if (!view.pinsLoaded) return;
  const pin = !view.pinned.includes(key);
  view.pinned = pin ? [...view.pinned, key] : view.pinned.filter((k) => k !== key);
  const mine = ++pinWrites;
  try {
    const prefs = await putUserPrefs(pin ? { pin: key } : { unpin: key });
    if (mine === pinWrites) view.pinned = prefs.pinned_workflows ?? [];
    view.pinError = null;
  } catch (e) {
    view.pinError = errorMessage(e);
    if (mine === pinWrites)
      view.pinned = pin ? view.pinned.filter((k) => k !== key) : [...view.pinned, key];
  }
}

/** The tour writes in flight, one after another. */
let tourSaved: Promise<void> = Promise.resolve();

/**
 * Record the tour as done or not. Writes go one at a time, and a write still waiting when a newer
 * one is asked for is dropped, so the server ends with this page's latest answer (a replay then a
 * quick skip never leaves it cleared). A refused write changes nothing here: the page keeps its
 * answer, and the next visit may ask again.
 */
function saveTour(done: boolean): Promise<void> {
  const mine = ++tourWrites;
  tourSaved = tourSaved.then(async () => {
    if (mine !== tourWrites) return;
    try {
      await putUserPrefs({ tour_done: done });
    } catch {
      /* kept for this page regardless */
    }
  });
  return tourSaved;
}

/**
 * Finish (or skip) the first-visit tour: it stops showing at once and is recorded for this user. A
 * refused save still ends it for this page; the next visit may then offer it again.
 */
export function finishTour(): Promise<void> {
  view.tourDone = true;
  return saveTour(true);
}

/** Replay the tour: shown again on My work in the Lab view, and recorded as not done. */
export function replayTour(): Promise<void> {
  view.tourDone = false;
  return saveTour(false);
}
