// The mock's own state beyond its fixtures: where it serves, who calls it, and the in-memory stores
// its writes change for the rest of the process (the fixtures under `fixtures/` change too: a
// launch adds a run, an upload an object, a rename names a run).

// The port to serve on (0: any free one; the chosen origin is logged, for the contract test).
export const PORT = Number(process.env.MOCK_PORT ?? 8080);
/** The origin the mock serves on, for the URLs it hands out (S3 parts, alignment files). */
export let ORIGIN = `http://localhost:${PORT}`;
/** Once listening: the origin it serves on (its port when `PORT` is 0). */
export function setOrigin(origin) {
  ORIGIN = origin;
}

/** The mock's one caller, as a run records its submitter. */
export const ME = 'user:alice';

/** The mock's one caller's id (the token's `sub`), as the backend records who made something
 * (`created_by` on experiments, pipeline versions and reference bundles). */
export const ME_SUB = 'alice';

/** The caller's label, as a run records who submitted or cancelled it. */
export const ME_LABEL = 'alice@lab.example.org';

/** The caller's preferences (`/me/prefs`): no view stored until one is chosen. */
export const PREFS = { pinned_workflows: [] };

// Running runs whose cancel is recorded (run id → who asked): they end as cancelled once their
// coordinator's end is "folded in", which the mock does on the run's next read (see the cancel
// route).
export const CANCEL_REQUESTED = new Map();

// The caller's notifications read, by the backend's id `<run id>:<state>` (see `handlers/me.mjs`):
// at start her failed and her newest finished analysis are unread, the older ones read.
export const NOTIFICATIONS_READ = new Set([
  'run_01HGW6workgone:succeeded',
  'run_01HGW4resume:succeeded',
  'run_01HGW0cancel:cancelled',
]);
// The failed runs the caller has dismissed from *Needs attention*, by notification id (the
// backend's `notification_dismissals`): a Dismiss, or a try-again of the run. Apart from reads:
// opening a run reads it and leaves it needing attention.
export const NOTIFICATIONS_DISMISSED = new Set();

// Pipelines a developer hid from the Lab listing (`PUT /workflows/listing`), by `project\u0000name`.
export const HIDDEN = new Set();

// Browser uploads in flight: multipart uploads by id (see `handlers/uploads.mjs`).
export const UPLOADS = new Map();
