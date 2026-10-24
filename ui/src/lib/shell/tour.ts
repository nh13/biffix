// The first-visit tour: four stops on My work, each a small popover anchored
// to what it points at. Pure logic only (the stops, when the tour shows, where a popover goes), so
// it is unit-tested apart from the component that draws it.

import type { ViewMode } from '#lib/view/viewRoutes.js';

/**
 * The `data-tour` values the stops point at. The elements carry them through these constants
 * (`data-tour={TOUR_ANCHOR.start}`), so a renamed anchor is a compile error, not a stop that
 * quietly points at nothing.
 */
export const TOUR_ANCHOR = {
  /** My work's *Start an analysis*. */
  start: 'start',
  /** My work's *In progress* row. */
  running: 'running',
  /** My work's welcome, shown instead of the rows to someone with no analyses yet. */
  welcome: 'welcome',
  /** The sidebar's Pipelines link. */
  navPipelines: 'nav-pipelines',
  /** The top bar's menu button (a narrow screen's way to the sidebar). */
  menu: 'menu',
  /** *Open sidebar* (a wide screen's way to a collapsed sidebar). */
  reopen: 'reopen',
  /** The Lab | Expert switch. */
  viewToggle: 'view-toggle',
} as const;
export type TourAnchor = (typeof TOUR_ANCHOR)[keyof typeof TOUR_ANCHOR];

export interface TourStop {
  /** The anchors of the element the stop points at, in order of preference. */
  anchors: readonly TourAnchor[];
  title: string;
  body: string;
}

export const TOUR_STOPS: readonly TourStop[] = [
  {
    anchors: [TOUR_ANCHOR.start],
    title: 'Start an analysis',
    body: 'Pick a pipeline, add your samples, and biffix runs it for you. Nothing runs until you click Launch analysis.',
  },
  {
    anchors: [TOUR_ANCHOR.running, TOUR_ANCHOR.welcome],
    title: 'Follow what is running',
    body: 'Your analyses show here while they run, sample by sample. Finished ones move to Recent results with their QC.',
  },
  {
    // The sidebar's link; else the way to the sidebar: the menu on a narrow screen, Open sidebar
    // when it is collapsed on a wide one.
    anchors: [TOUR_ANCHOR.navPipelines, TOUR_ANCHOR.menu, TOUR_ANCHOR.reopen],
    title: 'Find a pipeline',
    body: 'Pipelines, in the sidebar, lists the analyses your team has set up, each with a plain description. Pin the ones you use to keep them on My work.',
  },
  {
    anchors: [TOUR_ANCHOR.viewToggle],
    title: 'Lab or Expert',
    body: 'Lab shows samples and results. Expert shows the jobs, logs and costs behind them, for whoever helps you debug. Switch at any time.',
  },
];

/** Whether the tour shows: once the preferences say it is not done, in the Lab view, on My work. */
export function shouldShowTour(path: string, mode: ViewMode, tourDone: boolean | null): boolean {
  return path === '/' && mode === 'lab' && tourDone === false;
}

export interface Box {
  top: number;
  left: number;
  width: number;
  height: number;
}

/** Where a popover goes, in viewport pixels; `centred` when it has no anchor to point at. */
export interface Placement {
  top: number;
  left: number;
  centred: boolean;
}

/** The gap between an anchor and its popover, and between a popover and the viewport's edge. */
const GAP = 10;
const MARGIN = 8;

/**
 * Where a popover of `size` goes for an anchor at `anchor` (both viewport boxes) in a viewport of
 * `viewport`: below the anchor when it fits, else above, else (or with no anchor in view) centred.
 * Horizontally it lines up with the anchor and is kept inside the viewport.
 */
export function placePopover(
  anchor: Box | null,
  size: { width: number; height: number },
  viewport: { width: number; height: number },
): Placement {
  const clampLeft = (left: number) =>
    Math.max(MARGIN, Math.min(left, viewport.width - size.width - MARGIN));
  const centred = {
    top: Math.max(MARGIN, (viewport.height - size.height) / 2),
    left: clampLeft((viewport.width - size.width) / 2),
    centred: true,
  };
  const inView =
    anchor &&
    anchor.width > 0 &&
    anchor.height > 0 &&
    anchor.top < viewport.height &&
    anchor.top + anchor.height > 0;
  if (!inView) return centred;
  // Lined up with the anchor's left edge, or its right edge when it sits on the right half.
  const left = clampLeft(
    anchor.left + anchor.width / 2 > viewport.width / 2
      ? anchor.left + anchor.width - size.width
      : anchor.left,
  );
  const below = anchor.top + anchor.height + GAP;
  if (below + size.height <= viewport.height - MARGIN) return { top: below, left, centred: false };
  const above = anchor.top - GAP - size.height;
  if (above >= MARGIN) return { top: above, left, centred: false };
  return centred;
}
