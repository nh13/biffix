// Which pipeline version a run ran, as a person tells versions apart: the version's label (the
// one scheme `versionLabel` defines: short commit, content hash and build) and the day it was
// registered, linked to that exact version's page.

import type { RunDetail } from '#lib/api/index.js';
import { localDate } from '#lib/util/date.js';
import { versionLabelOfId } from '#lib/workflows/version.js';
import { workflowHref } from '#lib/workflows/workflowNav.js';

type RunVersion = Pick<RunDetail, 'workflow' | 'workflow_registered_at_micros'>;

/**
 * "version 1a2b3c4d5e:0f1e2d3c4b5a:0 · registered 2026-10-03" (the date only when the server knows
 * it); null for a run with no version.
 */
export function runVersionLabel(run: RunVersion): string | null {
  const id = run.workflow;
  if (!id) return null;
  const parts = [`version ${versionLabelOfId(id)}`];
  const at = run.workflow_registered_at_micros;
  if (at != null) parts.push(`registered ${localDate(new Date(at / 1000))}`);
  return parts.join(' · ');
}

/** The page of the exact version `run` ran; null for a run with no version. */
export function runVersionHref(run: Pick<RunDetail, 'workflow'>): string | null {
  return run.workflow ? workflowHref(run.workflow) : null;
}
