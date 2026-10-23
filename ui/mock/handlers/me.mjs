// The caller's own routes (me.rs): her preferences, her notifications and *Needs attention*.
import { RUNS } from '../fixtures/runs.mjs';
import { readJson } from '../lib/http.mjs';
import { badLimit, bytewise, clampInt } from '../lib/paging.mjs';
import { TERMINAL } from '../lib/run-states.mjs';
import { sampleCounts, sampleGrid } from '../lib/sample-grid.mjs';
import { ME, NOTIFICATIONS_DISMISSED, NOTIFICATIONS_READ, PREFS } from '../state.mjs';
import { workflowTitleOf } from './workflows.mjs';

// The caller's notifications, as the backend's `notifications` projection holds them: one per
// ended run of hers (its terminal kind, at its end), newest first, with the backend's id
// `<run id>:<state>`. Read state is per caller and kept here; at start her failed and her newest
// finished analysis are unread, the older ones read. A run she cancels herself is read ahead of
// its end (the cancel route), so it never needs her attention.
const notificationId = (r) => `${r.run_id}:${r.state}`;
/** The caller's ended runs: one notification each. */
const endedRunsOf = (caller) =>
  RUNS.filter((r) => r.submitter === caller && TERMINAL.has(r.state) && r.completed_at != null);
function notificationsOf(caller) {
  return endedRunsOf(caller)
    .map((r) => {
      const counts = sampleCounts(sampleGrid(r));
      const title = workflowTitleOf(r.workflow);
      return {
        id: notificationId(r),
        run_id: r.run_id,
        ...(r.run_name ? { run_name: r.run_name } : {}),
        ...(r.workflow_name ? { workflow_name: r.workflow_name } : {}),
        ...(title ? { workflow_title: title } : {}),
        ...(r.submitted_at != null ? { submitted_at: r.submitted_at } : {}),
        kind: r.state,
        at: r.completed_at,
        read: NOTIFICATIONS_READ.has(notificationId(r)),
        ...(counts ? { samples_total: counts.total, samples_failed: counts.failed } : {}),
      };
    })
    .sort((a, b) => b.at - a.at || bytewise(b.id, a.id));
}
function unreadCounts() {
  const unread = notificationsOf(ME).filter((n) => !n.read);
  return {
    unread: unread.length,
    unread_failed: unread.filter((n) => n.kind === 'failed').length,
  };
}
/** Mark every notification of `runId` read for the caller (a try-again of it, or opening it). */
export function readRun(runId) {
  for (const r of endedRunsOf(ME))
    if (r.run_id === runId) NOTIFICATIONS_READ.add(notificationId(r));
}
/** Dismiss the caller's failed runs `runIds` (those that are not hers, or did not fail, are not). */
export function dismissRuns(runIds) {
  for (const r of endedRunsOf(ME))
    if (r.state === 'failed' && runIds.includes(r.run_id))
      NOTIFICATIONS_DISMISSED.add(notificationId(r));
}
/** A failed run of the caller's she has not dismissed: *Needs attention*. */
export const needsAttention = (r) =>
  r.submitter === ME && r.state === 'failed' && !NOTIFICATIONS_DISMISSED.has(notificationId(r));

/** `GET /me/prefs`. */
function getPrefs({ send }) {
  send(200, PREFS);
}

// store/settings.rs: the most pins a caller keeps, and the longest pin key.
const MAX_PINS = 50;
const MAX_PIN_CHARS = 512;
/** me.rs `pin_key`: one pin key trimmed, or the 400 naming it as `field`. */
function pinKey(field, pin) {
  const key = pin.trim();
  const chars = [...key].length;
  if (chars > 0 && chars <= MAX_PIN_CHARS) return { key };
  const cut = [...pin].slice(0, 40).join('');
  return {
    error: `${field}: \`${cut.length < pin.length ? `${cut}…` : pin}\` must be 1 to ${MAX_PIN_CHARS} characters`,
  };
}
/** me.rs `normalize_update`: the update's pin change checked and trimmed, or the 400's message. */
function normalizeUpdate(u) {
  if (u.pin != null && u.unpin != null)
    return { error: 'give at most one of `pin` and `unpin` in one update' };
  const out = {};
  for (const field of ['pin', 'unpin'])
    if (u[field] != null) {
      const { key, error } = pinKey(field, u[field]);
      if (error) return { error };
      out[field] = key;
    }
  return { update: out };
}

/** `PUT /me/prefs`: the caller's own preferences, a partial update, as `me.rs` does it: the whole
 * update is checked before any of it is written, so a refused one changes nothing. */
function putPrefs({ req, send }) {
  readJson(req, send, (u) => {
    // An unknown view is a body the typed `ViewMode` refuses: axum answers 422.
    if (u.view != null && !['lab', 'expert'].includes(u.view))
      return send(422, `unknown variant \`${u.view}\`, expected \`lab\` or \`expert\``);
    const { update, error } = normalizeUpdate(u);
    if (error) return send(400, error);
    // `pin`/`unpin`: one operation on the stored list (append if absent / remove); a pin past the
    // cap is the store's conflict, before anything is written.
    let pins = PREFS.pinned_workflows;
    if (update.pin != null && !pins.includes(update.pin)) {
      if (pins.length >= MAX_PINS) return send(400, `pin: at most ${MAX_PINS} pinned pipelines`);
      pins = [...pins, update.pin];
    }
    if (update.unpin != null) pins = pins.filter((k) => k !== update.unpin);
    if (u.view != null) PREFS.view = u.view;
    PREFS.pinned_workflows = pins;
    if (u.tour_done === true) PREFS.tour_done_at = Date.now() / 1000;
    if (u.tour_done === false) delete PREFS.tour_done_at;
    send(200, PREFS);
  });
}

/** `GET /me/notifications`: the caller's, newest first, and how many of all of hers are unread. */
function listNotifications({ query, send }) {
  const limit = clampInt(query.get('limit'), 20, 1, 100);
  if (limit == null) return send(...badLimit(query.get('limit'), false));
  send(200, { items: notificationsOf(ME).slice(0, limit), ...unreadCounts() });
}

/** `POST /me/notifications/read`: mark some read, exactly one of `ids`, `run_ids` or `all: true`. */
function readNotifications({ req, send }) {
  readJson(req, send, (r) => {
    const given = [r.ids != null, r.run_ids != null, r.all === true].filter(Boolean).length;
    if (given !== 1) return send(400, 'give exactly one of ids, run_ids or all');
    for (const n of notificationsOf(ME))
      if (r.all === true || r.ids?.includes(n.id) || r.run_ids?.includes(n.run_id))
        NOTIFICATIONS_READ.add(n.id);
    send(200, unreadCounts());
  });
}

/** `POST /me/attention/dismiss`: dismiss exactly the named runs from *Needs attention* (1 to 500),
 * not reading them; answers how many still need attention. */
function dismissAttention({ req, send }) {
  readJson(req, send, (r) => {
    if (!Array.isArray(r.run_ids)) return send(422, 'missing field `run_ids`');
    if (r.run_ids.length > 500)
      return send(400, `\`run_ids\` names ${r.run_ids.length} entries; at most 500 per request`);
    const ids = r.run_ids.map((id) => String(id).trim()).filter(Boolean);
    if (!ids.length) return send(400, 'nothing selected: give the `run_ids` to dismiss');
    dismissRuns(ids);
    send(200, { attention: RUNS.filter(needsAttention).length });
  });
}

export const ME_ROUTES = [
  ['GET', '/me/prefs', getPrefs],
  ['PUT', '/me/prefs', putPrefs],
  ['GET', '/me/notifications', listNotifications],
  ['POST', '/me/notifications/read', readNotifications],
  ['POST', '/me/attention/dismiss', dismissAttention],
];
