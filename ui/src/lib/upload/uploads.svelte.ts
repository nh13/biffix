// Browser uploads into the project bucket. Each file is an S3 multipart
// upload the browser sends straight to S3: the API starts it, presigns part URLs, and assembles the
// parts; the bytes never pass through the API. Four parts go at once, each retried with backoff. A
// reload loses the File, so what is needed to resume is kept in localStorage and the tray asks for
// the same file again; S3 already holds the parts that finished.
//
// localStorage is shared by every tab of the app, so a tab holds a Web Lock per upload it sends
// (released when the upload ends, or when the tab closes or crashes). A saved upload is
// "interrupted" only when no tab holds its lock; one another tab holds is that tab's, and this tab
// neither discards nor resumes it.
//
// The network is behind `UploadTransport` so the engine is unit-tested without a server.

import { AuthRequiredError, type StartedUpload } from '#lib/api/index.js';
import { errorMessage } from '#lib/util/format.js';
import { readPref, writePref } from '#lib/util/storage.js';

/** Parts sent at once, retries of one part, and part URLs asked for at a time. */
export const CONCURRENCY = 4;
export const RETRIES = 3;
const URL_BATCH = 20;

/**
 * What the engine needs from the API and from S3. `project` is the project whose bucket the upload
 * goes into; absent, the deploy's default project.
 */
export interface UploadTransport {
  start(key: string, size: number, contentType: string, project?: string): Promise<StartedUpload>;
  partUrls(
    uploadId: string,
    key: string,
    parts: number[],
    project?: string,
  ): Promise<Map<number, string>>;
  /** Parts S3 already holds: part number → ETag. Rejects with `UploadGone` for an unknown
   * upload. */
  status(uploadId: string, key: string, project?: string): Promise<Map<number, string>>;
  complete(
    uploadId: string,
    key: string,
    parts: [number, string][],
    project?: string,
  ): Promise<{ uri: string }>;
  abort(uploadId: string, key: string, project?: string): Promise<void>;
  /** PUT one part; resolves to its ETag. Rejects with `PartUrlExpired` on a 403. */
  putPart(url: string, body: Blob, signal: AbortSignal): Promise<string>;
  /** Whether an object of `size` bytes is at `uri` (false when it cannot be told). */
  landed(uri: string, size: number, project?: string): Promise<boolean>;
}

/** The upload no longer exists (completed, aborted, or cleaned up). */
export class UploadGone extends Error {}
/** A presigned part URL was refused (it expired): ask for a fresh one. */
export class PartUrlExpired extends Error {}
/**
 * An object already exists at the key (a 409): uploads never replace one. From `start`, nothing
 * was sent (offer another name); from `complete`, the backend cancelled the upload.
 */
export class KeyTaken extends Error {}

/** What survives a reload, to resume once the same file is chosen again. */
export interface SavedUpload {
  upload_id: string;
  key: string;
  uri: string;
  name: string;
  size: number;
  last_modified: number;
  part_size: number;
  parts: number;
  /** The project whose bucket it goes into; absent for the deploy's default project. */
  project?: string;
}

const STORAGE_KEY = 'biffix:uploads';

export function loadSaved(): SavedUpload[] {
  const raw = readPref(STORAGE_KEY);
  try {
    const list = raw ? (JSON.parse(raw) as SavedUpload[]) : [];
    return Array.isArray(list) ? list : [];
  } catch {
    return [];
  }
}
/** When storage is unavailable, the upload just cannot resume after a reload. */
function writeSaved(list: SavedUpload[]) {
  writePref(STORAGE_KEY, JSON.stringify(list));
}
function save(u: SavedUpload) {
  writeSaved([...loadSaved().filter((s) => s.upload_id !== u.upload_id), u]);
}
function forget(uploadId: string) {
  writeSaved(loadSaved().filter((s) => s.upload_id !== uploadId));
}

// -- Which tab owns an upload (Web Locks) ---------------------------------------------------------

const LOCK_PREFIX = 'biffix:upload:';
type Release = () => void;
const noRelease: Release = () => {};

function lockManager(): LockManager | undefined {
  return (globalThis as { navigator?: Navigator }).navigator?.locks;
}

/**
 * Take the upload's lock for this tab until the returned function releases it; `null` when another
 * tab holds it. Without Web Locks every tab owns every upload.
 */
function claim(uploadId: string): Promise<Release | null> {
  const locks = lockManager();
  if (!locks) return Promise.resolve(noRelease);
  return new Promise((resolve) => {
    locks
      .request(LOCK_PREFIX + uploadId, { ifAvailable: true }, (lock) => {
        if (!lock) {
          resolve(null);
          return undefined;
        }
        // Held until released, or until the tab goes away.
        return new Promise<void>((release) => resolve(() => release()));
      })
      .catch(() => resolve(noRelease));
  });
}

/** Ids of the uploads some tab (this one included) holds the lock of. */
async function heldUploads(): Promise<Set<string>> {
  const locks = lockManager();
  if (!locks) return new Set();
  try {
    const { held = [] } = await locks.query();
    return new Set(
      held
        .map((l) => l.name ?? '')
        .filter((name) => name.startsWith(LOCK_PREFIX))
        .map((name) => name.slice(LOCK_PREFIX.length)),
    );
  } catch {
    return new Set();
  }
}

/** Resolves once no tab holds the upload's lock (its owner ended it, or went away). */
function whenReleased(uploadId: string): Promise<void> {
  const locks = lockManager();
  if (!locks) return new Promise(() => {});
  return locks
    .request(LOCK_PREFIX + uploadId, () => undefined)
    .then(
      () => undefined,
      () => undefined,
    );
}

/** Whether `file` is the file a saved upload was started from. */
export function sameFile(saved: SavedUpload, file: File): boolean {
  return (
    saved.name === file.name &&
    saved.size === file.size &&
    saved.last_modified === file.lastModified
  );
}

export type UploadState = 'uploading' | 'paused' | 'completing' | 'done' | 'failed' | 'cancelled';

/**
 * A file a form value names that S3 does not hold yet. `uploading`, `paused`, `failed`: this tab
 * sends it (a `failed` one may finish on Retry); `lost`: it failed for good (the backend no longer
 * has the upload), so it must be uploaded again or another file chosen; `interrupted`: a reload
 * stopped it, and it resumes once the file is chosen again; `elsewhere`: another tab sends it.
 */
export interface PendingUpload {
  name: string;
  stage: 'uploading' | 'paused' | 'failed' | 'lost' | 'interrupted' | 'elsewhere';
  /** The percent S3 holds, for an upload this tab sends; `null` otherwise. */
  percent: number | null;
}

/** Why the launch form cannot use the file yet. */
export function pendingProblem(p: PendingUpload): string {
  switch (p.stage) {
    case 'uploading':
      return `${p.name} is still uploading (${p.percent}%)`;
    case 'paused':
      return `${p.name} is paused at ${p.percent}%; resume it in Uploads`;
    case 'failed':
      return `${p.name} failed to upload; retry it in Uploads`;
    case 'lost':
      return `${p.name} was not uploaded; upload it again or choose another file`;
    case 'interrupted':
      return `${p.name} must be chosen again to finish uploading`;
    case 'elsewhere':
      return `${p.name} is still uploading in another tab`;
  }
}

/** A file field's note on the upload of its file. */
export function pendingNote(p: PendingUpload): string {
  switch (p.stage) {
    case 'uploading':
      return `Uploading ${p.name}… ${p.percent}%`;
    case 'interrupted':
      return `Waiting for ${p.name} to be chosen again (see Uploads)`;
    default:
      return pendingProblem(p);
  }
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** One file's upload: its progress, and pause, resume and cancel. */
export class Upload {
  state = $state<UploadState>('uploading');
  /** Bytes S3 holds. */
  sent = $state(0);
  error = $state<string | null>(null);
  /** Bytes per second since this session of the upload began. */
  rate = $state(0);
  /**
   * Failed for good: the backend no longer has the upload (another file reached the key, or it
   * expired), so Retry cannot work; the file must be uploaded again.
   */
  lost = $state(false);
  readonly name: string;
  readonly size: number;
  /** Releases this tab's claim on the upload (its Web Lock); called once the upload ends. */
  release: Release = noRelease;

  #etags = new Map<number, string>();
  #urls = new Map<number, string>();
  /** The part-URL request in flight, which workers that also need URLs wait on. */
  #fetchingUrls: Promise<void> | null = null;
  #controller: AbortController | null = null;
  #sessionStart = 0;
  #sessionSent = 0;
  /** Whether `#etags` is what S3 holds; not yet for an upload resumed after a reload. */
  #statusKnown = true;
  /** A `complete` failed without the backend's answer: it may have assembled the file anyway. */
  #completeUnanswered = false;

  constructor(
    readonly saved: SavedUpload,
    private file: File,
    private transport: UploadTransport,
    private backoff: (attempt: number) => number = (a) => 1000 * 2 ** a,
  ) {
    this.name = saved.name;
    this.size = saved.size;
  }

  get uri(): string {
    return this.saved.uri;
  }
  /** Nothing more can happen to it: done, cancelled, or lost. */
  get finished(): boolean {
    return this.state === 'done' || this.state === 'cancelled' || this.lost;
  }
  get fraction(): number {
    return this.size === 0 ? (this.state === 'done' ? 1 : 0) : this.sent / this.size;
  }

  #partBlob(n: number): Blob {
    const start = (n - 1) * this.saved.part_size;
    return this.file.slice(start, Math.min(start + this.saved.part_size, this.size));
  }

  /** Learn which parts S3 already holds (a resumed upload), then send the rest. */
  async resume(): Promise<void> {
    this.#statusKnown = false;
    await this.run();
  }

  /** The upload has ended: nothing to resume after a reload, and other tabs may look again. */
  #end() {
    forget(this.saved.upload_id);
    this.release();
    this.release = noRelease;
  }

  /** S3 holds the whole file. */
  #done() {
    this.#end();
    this.sent = this.size;
    this.state = 'done';
  }

  /**
   * Send every part S3 does not hold yet, then assemble the file. Pause and Cancel work from the
   * start, including while a resumed upload asks which parts S3 holds.
   */
  async run(): Promise<void> {
    if (this.finished) return;
    this.state = 'uploading';
    this.error = null;
    const controller = new AbortController();
    this.#controller = controller;
    // Part URLs expire (15 minutes): one from before a pause or a failure is likely stale.
    this.#urls.clear();
    try {
      if (!this.#statusKnown) {
        const held = await this.transport.status(
          this.saved.upload_id,
          this.saved.key,
          this.saved.project,
        );
        // A newer session (paused, then resumed meanwhile) asked again: its answer is the one kept.
        if (this.#controller !== controller) return;
        // Kept even when paused meanwhile, so Resume sends only what S3 lacks.
        this.#etags = held;
        this.sent = [...held.keys()].reduce((n, p) => n + this.#partBlob(p).size, 0);
        this.#statusKnown = true;
        if (controller.signal.aborted) return;
      }
      this.#sessionStart = Date.now();
      this.#sessionSent = 0;
      const todo: number[] = [];
      for (let n = 1; n <= this.saved.parts; n++) if (!this.#etags.has(n)) todo.push(n);
      await Promise.all(
        Array.from({ length: Math.min(CONCURRENCY, todo.length) }, () =>
          this.#worker(todo, controller.signal),
        ),
      );
      if (controller.signal.aborted) return;
      this.state = 'completing';
      const parts = [...this.#etags.entries()].sort((a, b) => a[0] - b[0]);
      await this.transport
        .complete(this.saved.upload_id, this.saved.key, parts, this.saved.project)
        .catch((e) => {
          if (!(e instanceof KeyTaken || e instanceof UploadGone)) this.#completeUnanswered = true;
          throw e;
        });
      this.#done();
    } catch (e) {
      if (controller.signal.aborted) return;
      controller.abort();
      // The backend no longer has the upload (or, after a lost answer, the key holds an object):
      // it can be this upload's own, assembled by a `complete` whose answer was lost, or before a
      // reload.
      const mayHaveLanded =
        e instanceof UploadGone || (e instanceof KeyTaken && this.#completeUnanswered);
      const landed = mayHaveLanded && (await this.#landed());
      // Cancelled while that was asked.
      if ((this.state as UploadState) === 'cancelled') return;
      if (landed) {
        this.#done();
        return;
      }
      this.state = 'failed';
      if (e instanceof KeyTaken) {
        // Another file reached the key first; the backend cancelled this upload, so nothing is
        // left to resume.
        this.lost = true;
        this.#end();
        this.error = `${e.message} (nothing was replaced; this upload was cancelled)`;
      } else if (e instanceof UploadGone) {
        // Completed, aborted or cleaned up already: there is nothing left to send parts to.
        this.lost = true;
        this.#end();
        this.error = 'This upload expired; upload the file again.';
      } else {
        this.error = errorMessage(e);
      }
    }
  }

  /**
   * Whether the key holds an object of this upload's size. Its size is the evidence: `start`
   * refuses a key that already holds an object, so one there now arrived during this upload.
   */
  async #landed(): Promise<boolean> {
    try {
      return await this.transport.landed(this.saved.uri, this.size, this.saved.project);
    } catch {
      return false;
    }
  }

  async #worker(todo: number[], signal: AbortSignal): Promise<void> {
    while (!signal.aborted) {
      const n = todo.shift();
      if (n === undefined) return;
      const blob = this.#partBlob(n);
      let attempt = 0;
      let refreshed = false;
      for (;;) {
        try {
          const url = await this.#urlFor(n, todo);
          const etag = await this.transport.putPart(url, blob, signal);
          // A part a paused session's late PUT already delivered is not counted twice.
          if (!this.#etags.has(n)) {
            this.sent += blob.size;
            this.#sessionSent += blob.size;
          }
          this.#etags.set(n, etag);
          const secs = (Date.now() - this.#sessionStart) / 1000;
          this.rate = secs > 0 ? this.#sessionSent / secs : 0;
          break;
        } catch (e) {
          if (signal.aborted) return;
          // No retry can help: the upload is gone, or the session ended (signing in again).
          if (e instanceof UploadGone || e instanceof AuthRequiredError) throw e;
          // A refused URL has expired: ask for a fresh one at once, the first time without
          // spending a retry on it.
          if (e instanceof PartUrlExpired) {
            this.#urls.delete(n);
            if (!refreshed) {
              refreshed = true;
              continue;
            }
          }
          if (++attempt > RETRIES) throw e;
          await sleep(this.backoff(attempt - 1));
        }
      }
    }
  }

  /** The part's URL, asking for it (and the next few waiting parts) when not yet known. */
  async #urlFor(n: number, waiting: number[]): Promise<string> {
    // Another worker's request may bring this part's URL.
    while (!this.#urls.has(n) && this.#fetchingUrls) await this.#fetchingUrls;
    const known = this.#urls.get(n);
    if (known) return known;
    const batch = [n, ...waiting.filter((p) => !this.#urls.has(p)).slice(0, URL_BATCH - 1)];
    const fetching = this.transport
      .partUrls(this.saved.upload_id, this.saved.key, batch, this.saved.project)
      .then((urls) => {
        for (const [p, u] of urls) this.#urls.set(p, u);
      });
    this.#fetchingUrls = fetching;
    try {
      await fetching;
    } finally {
      if (this.#fetchingUrls === fetching) this.#fetchingUrls = null;
    }
    const url = this.#urls.get(n);
    if (!url) throw new Error(`no URL for part ${n}`);
    return url;
  }

  /** Stop sending; the parts already sent stay in S3. */
  pause() {
    if (this.state !== 'uploading') return;
    this.#controller?.abort();
    this.state = 'paused';
  }

  /** Abandon the upload: S3 deletes its parts. */
  async cancel(): Promise<void> {
    if (this.finished) return;
    this.#controller?.abort();
    this.state = 'cancelled';
    try {
      await this.transport.abort(this.saved.upload_id, this.saved.key, this.saved.project);
    } catch {
      /* already gone, or the lifecycle rule will delete the parts */
    } finally {
      this.#end();
    }
  }
}

/**
 * The uploads of this browser tab, for the tray and for the launch form (which waits for any file
 * it names that is still uploading). One per app.
 */
export class Uploads {
  list = $state<Upload[]>([]);
  /** Uploads a reload interrupted (no tab sends them): they resume once their file is chosen
   * again. */
  interrupted = $state<SavedUpload[]>([]);
  /** Uploads another tab of the app sends: not this tab's to resume or discard. */
  elsewhere = $state<SavedUpload[]>([]);

  /** Uploads being taken up here (`resumeWith`, `discard`) while their lock is asked for. */
  #adopting = new Set<string>();
  /** Other tabs' uploads this tab waits on, to look again once each ends. */
  #watching = new Set<string>();
  #listening = false;

  constructor(private transport: UploadTransport) {}

  /**
   * Sort the uploads saved for a reload into interrupted ones and other tabs' (call once the page
   * is up). It runs again whenever another tab changes what is saved, or ends one of its uploads.
   */
  async restore(): Promise<void> {
    this.#listen();
    // Saved first, then the locks: an upload is saved only once its tab holds its lock.
    const saved = loadSaved();
    const held = await heldUploads();
    const mine = new Set(this.list.map((u) => u.saved.upload_id));
    const others = saved.filter((s) => !mine.has(s.upload_id) && !this.#adopting.has(s.upload_id));
    this.interrupted = others.filter((s) => !held.has(s.upload_id));
    this.elsewhere = others.filter((s) => held.has(s.upload_id));
    for (const s of this.elsewhere) this.#watch(s.upload_id);
  }

  #listen() {
    if (this.#listening || typeof window === 'undefined') return;
    this.#listening = true;
    // Another tab started, finished or discarded an upload.
    window.addEventListener('storage', (e) => {
      if (e.key === STORAGE_KEY || e.key === null) void this.restore();
    });
  }

  /** Look again once another tab's upload ends (it finished, or its tab went away). */
  #watch(uploadId: string) {
    if (this.#watching.has(uploadId)) return;
    this.#watching.add(uploadId);
    void whenReleased(uploadId).then(() => {
      this.#watching.delete(uploadId);
      void this.restore();
    });
  }

  /**
   * Upload `file` to `key` in `project`'s bucket (the default project's when absent); resolves once
   * the upload has started.
   */
  async start(file: File, key: string, project?: string | null): Promise<Upload> {
    const started = await this.transport.start(
      key,
      file.size,
      file.type || 'application/octet-stream',
      project || undefined,
    );
    const saved: SavedUpload = {
      upload_id: started.upload_id,
      key: started.key,
      uri: started.uri,
      name: file.name,
      size: file.size,
      last_modified: file.lastModified,
      part_size: started.part_size,
      parts: started.parts,
      ...(project ? { project } : {}),
    };
    // A new upload id, so no other tab holds it. Claimed before it is saved, so no tab ever sees it
    // saved but unclaimed.
    const release = (await claim(saved.upload_id)) ?? noRelease;
    save(saved);
    const upload = this.#add(saved, file, release);
    void upload.run();
    return upload;
  }

  /** Track an upload of `file` this tab holds the claim (`release`) of. */
  #add(saved: SavedUpload, file: File, release: Release): Upload {
    const upload = new Upload(saved, file, this.transport);
    upload.release = release;
    this.list = [...this.list, upload];
    return upload;
  }

  /**
   * Resume an interrupted upload with its file chosen again: `different-file` when `file` is not
   * the one it started from, `elsewhere` when another tab took it up meanwhile.
   */
  async resumeWith(
    saved: SavedUpload,
    file: File,
  ): Promise<'resumed' | 'different-file' | 'elsewhere'> {
    if (!sameFile(saved, file)) return 'different-file';
    const id = saved.upload_id;
    this.interrupted = this.interrupted.filter((s) => s.upload_id !== id);
    this.#adopting.add(id);
    try {
      const release = await claim(id);
      if (!release) {
        this.#toElsewhere(saved);
        return 'elsewhere';
      }
      void this.#add(saved, file, release).resume();
      return 'resumed';
    } finally {
      this.#adopting.delete(id);
    }
  }

  /** Forget an interrupted upload and delete its parts (unless another tab took it up
   * meanwhile). */
  async discard(saved: SavedUpload): Promise<void> {
    const id = saved.upload_id;
    this.interrupted = this.interrupted.filter((s) => s.upload_id !== id);
    this.#adopting.add(id);
    try {
      const release = await claim(id);
      if (!release) {
        this.#toElsewhere(saved);
        return;
      }
      forget(id);
      try {
        await this.transport.abort(id, saved.key, saved.project);
      } catch {
        /* already gone */
      } finally {
        release();
      }
    } finally {
      this.#adopting.delete(id);
    }
  }

  #toElsewhere(saved: SavedUpload) {
    this.elsewhere = [...this.elsewhere.filter((s) => s.upload_id !== saved.upload_id), saved];
    this.#watch(saved.upload_id);
  }

  /** Clear finished uploads from the tray. */
  clearFinished() {
    this.list = this.list.filter((u) => !u.finished);
  }

  /** The unfinished upload (this tab's, interrupted, or another tab's) that will become `uri`. */
  pendingFor(uri: string): PendingUpload | null {
    const tries = this.list.filter((u) => u.uri === uri && u.state !== 'cancelled');
    // S3 holds the file once any upload of it finished; else the newest one speaks for it (a
    // re-upload supersedes an older lost or failed one).
    if (tries.some((u) => u.state === 'done')) return null;
    const live = tries.at(-1);
    if (live) {
      if (live.lost) return { name: live.name, stage: 'lost', percent: null };
      const percent = Math.min(100, Math.floor(live.fraction * 100));
      const stage =
        live.state === 'failed' ? 'failed' : live.state === 'paused' ? 'paused' : 'uploading';
      return { name: live.name, stage, percent };
    }
    const interrupted = this.interrupted.find((s) => s.uri === uri);
    if (interrupted) return { name: interrupted.name, stage: 'interrupted', percent: null };
    const other = this.elsewhere.find((s) => s.uri === uri);
    if (other) return { name: other.name, stage: 'elsewhere', percent: null };
    return null;
  }
}
