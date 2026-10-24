import { beforeEach, describe, expect, it } from 'vitest';
import { AuthRequiredError } from '#lib/api/index.js';
import { storageWith } from '#lib/api/testing.js';
import {
  CONCURRENCY,
  KeyTaken,
  loadSaved,
  PartUrlExpired,
  pendingNote,
  pendingProblem,
  Upload,
  Uploads,
  UploadGone,
  type UploadTransport,
} from './uploads.svelte';

// Upload ids are unique across the file: Web Locks (one per upload) are process-wide here.
let nextId = 1;

/** An in-memory S3: parts by upload, with hooks to fail or hold a part. */
class FakeS3 implements UploadTransport {
  uploads = new Map<string, { key: string; parts: Map<number, Blob> }>();
  completed = new Map<string, [number, string][]>();
  aborted: string[] = [];
  urlRequests: number[][] = [];
  puts: number[] = [];
  inFlight = 0;
  maxInFlight = 0;
  /** Part number → failures still to give. */
  failures = new Map<number, Error[]>();
  partSize = 8;
  /** An aborted PUT still lands (its response was already on the way). */
  lateAborts = false;
  /** While set, `status` answers only once it resolves. */
  statusGate: Promise<void> | null = null;
  /** Keys an object already holds: uploads there are refused (409), as by the backend. */
  taken = new Set<string>();
  /** Each API call's name and the project it named (`undefined`: the default project). */
  projectsNamed: [string, string | undefined][] = [];
  /** The objects completed uploads made: URI → size. */
  objects = new Map<string, number>();
  /** The next `complete` assembles the object, but its answer is lost (a dropped connection). */
  loseCompleteAnswer = false;
  /** Part URLs carry the generation they were presigned in; those before `fresh` have expired. */
  urlGeneration = 0;
  fresh = 0;
  /** PUTs refused for an expired URL. */
  refused = 0;
  /** Every part URL presigned so far expires. */
  expireUrls() {
    this.fresh = ++this.urlGeneration;
  }

  async start(key: string, size: number, _contentType?: string, project?: string) {
    this.projectsNamed.push(['start', project]);
    if (this.taken.has(key)) throw new KeyTaken(`an object already exists at ${key}`);
    const upload_id = `u${nextId++}`;
    this.uploads.set(upload_id, { key, parts: new Map() });
    const parts = Math.max(1, Math.ceil(size / this.partSize));
    return { upload_id, key, uri: `s3://proj/${key}`, part_size: this.partSize, parts };
  }
  async partUrls(uploadId: string, _key: string, parts: number[], project?: string) {
    this.projectsNamed.push(['parts', project]);
    this.urlRequests.push(parts);
    return new Map(parts.map((p) => [p, `https://s3/${uploadId}/${p}/${this.urlGeneration}`]));
  }
  async status(uploadId: string, _key?: string, project?: string) {
    this.projectsNamed.push(['status', project]);
    // Answered as S3 is when asked; the answer may arrive late.
    const u = this.uploads.get(uploadId);
    const held = u && new Map([...u.parts.keys()].map((p) => [p, `etag-${p}`]));
    if (this.statusGate) await this.statusGate;
    if (!held) throw new UploadGone('gone');
    return held;
  }
  async complete(uploadId: string, key: string, parts: [number, string][], project?: string) {
    this.projectsNamed.push(['complete', project]);
    const u = this.uploads.get(uploadId);
    if (!u) throw new UploadGone('gone');
    if (this.taken.has(key)) {
      this.uploads.delete(uploadId);
      throw new KeyTaken(`an object already exists at ${key}`);
    }
    this.completed.set(uploadId, parts);
    this.uploads.delete(uploadId);
    this.objects.set(
      `s3://proj/${key}`,
      [...u.parts.values()].reduce((n, b) => n + b.size, 0),
    );
    if (this.loseCompleteAnswer) {
      this.loseCompleteAnswer = false;
      throw new Error('connection reset');
    }
    return { uri: 'x' };
  }
  async landed(uri: string, size: number) {
    return this.objects.get(uri) === size;
  }
  async abort(uploadId: string, _key?: string, project?: string) {
    this.projectsNamed.push(['abort', project]);
    this.aborted.push(uploadId);
    this.uploads.delete(uploadId);
  }
  async putPart(url: string, body: Blob, signal: AbortSignal) {
    const [, , , id, part, generation] = url.split('/');
    const n = Number(part);
    if (Number(generation) < this.fresh) {
      this.refused++;
      throw new PartUrlExpired('403');
    }
    this.inFlight++;
    this.maxInFlight = Math.max(this.maxInFlight, this.inFlight);
    try {
      await new Promise((r) => setTimeout(r, 5));
      if (signal.aborted && !this.lateAborts) throw new Error('aborted');
      const fail = this.failures.get(n)?.shift();
      if (fail) throw fail;
      this.puts.push(n);
      this.uploads.get(id)?.parts.set(n, body);
      return `etag-${n}`;
    } finally {
      this.inFlight--;
    }
  }
}

const file = (bytes: number, name = 'P01_R1.fastq.gz') =>
  new File([new Uint8Array(bytes).fill(65)], name, { lastModified: 1234 });

const tick = (ms = 2) => new Promise((r) => setTimeout(r, ms));

async function finished(u: Upload) {
  for (let i = 0; i < 500 && !['done', 'failed', 'cancelled', 'paused'].includes(u.state); i++)
    await tick(5);
}

/** The tab goes away: the browser releases its locks (the saved records stay). */
async function closeTab(tab: Uploads) {
  for (const u of tab.list) u.release();
  await tick();
}

// A tab's localStorage (the node test environment has none).
beforeEach(() => {
  (globalThis as { localStorage?: unknown }).localStorage = storageWith({});
});

describe('uploads', () => {
  it('sends every part, at most four at a time, and assembles them in order', async () => {
    const s3 = new FakeS3();
    const uploads = new Uploads(s3);
    const u = await uploads.start(file(80), 'uploads/a.fq.gz');
    // Saved so a reload can resume it.
    expect(loadSaved().map((s) => s.key)).toEqual(['uploads/a.fq.gz']);
    await finished(u);
    expect(u.state).toBe('done');
    expect(u.sent).toBe(80);
    expect(s3.maxInFlight).toBeLessThanOrEqual(CONCURRENCY);
    expect(s3.maxInFlight).toBeGreaterThan(1);
    expect(s3.completed.get(u.saved.upload_id)!.map(([n]) => n)).toEqual([
      1, 2, 3, 4, 5, 6, 7, 8, 9, 10,
    ]);
    // URLs are asked for in one batch, not one request per part or per worker.
    expect(s3.urlRequests).toEqual([[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]]);
    expect(loadSaved()).toEqual([]);
    expect(uploads.pendingFor('s3://proj/uploads/a.fq.gz')).toBeNull();
  });

  it('names the project of an upload into another project in every call, after a reload too', async () => {
    const s3 = new FakeS3();
    const first = new Uploads(s3);
    // The default project's upload names none.
    await finished(await first.start(file(8), 'uploads/default.fq.gz'));
    expect(new Set(s3.projectsNamed.map(([, p]) => p))).toEqual(new Set([undefined]));
    expect(loadSaved()).toEqual([]);

    s3.projectsNamed = [];
    const u = await first.start(file(80), 'uploads/bench.fq.gz', 'imported');
    expect(loadSaved()[0].project).toBe('imported');
    while (s3.puts.length < 2) await tick(1);
    u.pause();
    await closeTab(first);
    const reloaded = new Uploads(s3);
    await reloaded.restore();
    await reloaded.resumeWith(reloaded.interrupted[0], file(80));
    await finished(reloaded.list[0]);
    expect(reloaded.list[0].state).toBe('done');
    const calls = s3.projectsNamed.map(([call]) => call);
    expect(calls).toContain('status');
    expect(calls).toContain('complete');
    expect(s3.projectsNamed.every(([, p]) => p === 'imported')).toBe(true);
  });

  it('retries a failing part, and gives up after three retries', async () => {
    const s3 = new FakeS3();
    // Exactly three failures still succeed; a fourth gives up.
    s3.failures.set(
      2,
      Array.from({ length: 3 }, () => new Error('reset')),
    );
    const u = await startedUpload(s3, 24);
    await u.run();
    expect(u.state).toBe('done');

    const s3b = new FakeS3();
    s3b.failures.set(
      1,
      Array.from({ length: 4 }, () => new Error('network down')),
    );
    const v = await startedUpload(s3b, 8);
    await v.run();
    expect(v.state).toBe('failed');
    expect(v.error).toBe('network down');
    // A failure Retry may fix: not lost, not finished.
    expect(v.lost).toBe(false);
    expect(v.finished).toBe(false);
  });

  it('backs off between retries, longer each time', async () => {
    const s3 = new FakeS3();
    s3.failures.set(
      1,
      Array.from({ length: 3 }, () => new Error('reset')),
    );
    const waits: number[] = [];
    const u = await startedUpload(s3, 8, (a) => {
      waits.push(a);
      return 1;
    });
    await u.run();
    expect(u.state).toBe('done');
    expect(waits).toEqual([0, 1, 2]);
  });

  it('asks again for the URL of a part whose URL expired', async () => {
    const s3 = new FakeS3();
    s3.failures.set(1, [new PartUrlExpired('403')]);
    const u = await startedUpload(s3, 8);
    await u.run();
    expect(u.state).toBe('done');
    expect(s3.urlRequests).toEqual([[1], [1]]);
  });

  it('asks for fresh part URLs in batches once a pause outlived them', async () => {
    const s3 = new FakeS3();
    const uploads = new Uploads(s3);
    const u = await uploads.start(file(160), 'uploads/slow.fq.gz');
    while (s3.puts.length < 4) await tick(1);
    u.pause();
    await tick(20);
    s3.expireUrls();
    const asked = s3.urlRequests.length;
    await u.run();
    expect(u.state).toBe('done');
    expect(s3.refused).toBe(0);
    expect(s3.urlRequests[asked].length).toBeGreaterThan(1);
  });

  it('asks again at once, without spending a retry, when a URL expires mid-session', async () => {
    const s3 = new FakeS3();
    // Three failures and then an expired URL: a fourth failure would give up.
    s3.failures.set(1, [new Error('reset'), new Error('reset'), new Error('reset')]);
    const u = await startedUpload(s3, 8);
    const putPart = s3.putPart.bind(s3);
    let tries = 0;
    s3.putPart = async (...args) => {
      if (++tries === 4) s3.expireUrls();
      return putPart(...args);
    };
    await u.run();
    expect(u.state).toBe('done');
    expect(s3.refused).toBe(1);
  });

  it('pauses without losing sent parts, then sends only the rest', async () => {
    const s3 = new FakeS3();
    const uploads = new Uploads(s3);
    const u = await uploads.start(file(160), 'uploads/b.fq.gz');
    while (u.sent === 0) await tick();
    u.pause();
    expect(u.state).toBe('paused');
    await tick(20);
    const before = s3.puts.length;
    expect(before).toBeLessThan(20);
    await u.run();
    expect(u.state).toBe('done');
    // Each part was sent exactly once.
    expect(s3.puts).toHaveLength(20);
    expect(new Set(s3.puts).size).toBe(20);
  });

  it('never counts past 100% when a paused PUT lands after the upload resumed', async () => {
    const s3 = new FakeS3();
    s3.lateAborts = true;
    const uploads = new Uploads(s3);
    const u = await uploads.start(file(160), 'uploads/late.fq.gz');
    while (u.sent === 0) await tick(1);
    u.pause();
    const running = u.run();
    let most = 0;
    while (u.state !== 'done') {
      most = Math.max(most, u.sent);
      await tick(1);
    }
    await running;
    expect(most).toBeLessThanOrEqual(160);
    expect(u.fraction).toBe(1);
  });

  it('resumes after a reload once the same file is chosen again', async () => {
    const s3 = new FakeS3();
    const first = new Uploads(s3);
    const u = await first.start(file(80), 'uploads/c.fq.gz');
    // The tab reloads after two parts: the page's uploads are gone, the saved record is not.
    while (s3.puts.length < 2) await tick(1);
    u.pause();
    const id = u.saved.upload_id;
    const heldBefore = [...s3.uploads.get(id)!.parts.keys()];
    await closeTab(first);

    const reloaded = new Uploads(s3);
    await reloaded.restore();
    expect(reloaded.interrupted.map((s) => s.name)).toEqual(['P01_R1.fastq.gz']);
    expect(reloaded.pendingFor('s3://proj/uploads/c.fq.gz')).toEqual({
      name: 'P01_R1.fastq.gz',
      stage: 'interrupted',
      percent: null,
    });
    const saved = reloaded.interrupted[0];
    expect(await reloaded.resumeWith(saved, file(81))).toBe('different-file');
    s3.puts = [];
    expect(await reloaded.resumeWith(saved, file(80))).toBe('resumed');
    const resumed = reloaded.list[0];
    await finished(resumed);
    expect(resumed.state).toBe('done');
    // Only the parts S3 did not already hold were sent again.
    expect(s3.puts.every((p) => !heldBefore.includes(p))).toBe(true);
    expect(s3.completed.get(id)!.length).toBe(10);
  });

  it("another tab's live upload is not interrupted: not resumed, not discarded, waited for", async () => {
    const s3 = new FakeS3();
    const tabA = new Uploads(s3);
    const u = await tabA.start(file(80), 'uploads/shared.fq.gz');
    const uri = 's3://proj/uploads/shared.fq.gz';

    const tabB = new Uploads(s3);
    await tabB.restore();
    expect(tabB.interrupted).toEqual([]);
    expect(tabB.elsewhere.map((s) => s.name)).toEqual(['P01_R1.fastq.gz']);
    expect(tabB.pendingFor(uri)).toMatchObject({ stage: 'elsewhere' });
    // Neither a stale Discard nor a stale resume in tab B touches tab A's upload.
    const saved = loadSaved()[0];
    await tabB.discard(saved);
    expect(s3.aborted).toEqual([]);
    expect(await tabB.resumeWith(saved, file(80))).toBe('elsewhere');
    expect(tabB.list).toEqual([]);

    // Once tab A finishes, tab B looks again and no longer waits for it.
    await finished(u);
    expect(u.state).toBe('done');
    for (let i = 0; i < 100 && tabB.elsewhere.length > 0; i++) await tick();
    expect(tabB.elsewhere).toEqual([]);
    expect(tabB.interrupted).toEqual([]);
    expect(tabB.pendingFor(uri)).toBeNull();
  });

  it('an upload whose tab went away becomes interrupted in a tab still open', async () => {
    const s3 = new FakeS3();
    const tabA = new Uploads(s3);
    const u = await tabA.start(file(160), 'uploads/crash.fq.gz');
    u.pause();
    const tabB = new Uploads(s3);
    await tabB.restore();
    expect(tabB.elsewhere).toHaveLength(1);
    await closeTab(tabA);
    for (let i = 0; i < 100 && tabB.interrupted.length === 0; i++) await tick();
    expect(tabB.elsewhere).toEqual([]);
    expect(tabB.interrupted.map((s) => s.upload_id)).toEqual([u.saved.upload_id]);
  });

  it("pause and cancel during a resumed upload's status request stick", async () => {
    const s3 = new FakeS3();
    const first = new Uploads(s3);
    const u = await first.start(file(160), 'uploads/race.fq.gz');
    while (s3.puts.length < 4) await tick(1);
    u.pause();
    await tick(20);
    await closeTab(first);
    const held = s3.uploads.get(u.saved.upload_id)!.parts.size;

    // Paused while the status request is out: it stays paused, and Resume sends only the rest.
    const tab = new Uploads(s3);
    await tab.restore();
    let open!: () => void;
    s3.statusGate = new Promise<void>((r) => (open = r));
    expect(await tab.resumeWith(tab.interrupted[0], file(160))).toBe('resumed');
    const resumed = tab.list[0];
    resumed.pause();
    s3.puts = [];
    open();
    await tick(30);
    expect(resumed.state).toBe('paused');
    expect(s3.puts).toEqual([]);
    s3.statusGate = null;
    await resumed.run();
    expect(resumed.state).toBe('done');
    expect(s3.puts).toHaveLength(20 - held);

    // Cancelled while the status request is out: it stays cancelled.
    const other = new Uploads(s3);
    const v = await other.start(file(80), 'uploads/race2.fq.gz');
    v.pause();
    await closeTab(other);
    const tab2 = new Uploads(s3);
    await tab2.restore();
    s3.statusGate = new Promise<void>((r) => (open = r));
    await tab2.resumeWith(tab2.interrupted[0], file(80));
    const w = tab2.list[0];
    await w.cancel();
    open();
    await tick(30);
    expect(w.state).toBe('cancelled');
    expect(w.error).toBeNull();
  });

  it("a paused session's late status answer never overwrites a newer session's", async () => {
    const s3 = new FakeS3();
    const first = new Uploads(s3);
    const u = await first.start(file(80), 'uploads/late.fq.gz');
    while (s3.puts.length < 2) await tick(1);
    u.pause();
    await tick(20);
    await closeTab(first);
    const tab = new Uploads(s3);
    await tab.restore();
    let open!: () => void;
    s3.statusGate = new Promise<void>((r) => (open = r));
    await tab.resumeWith(tab.interrupted[0], file(80));
    const resumed = tab.list[0];
    resumed.pause();
    // Resumed again while the first status request is still out: this one finishes the upload.
    s3.statusGate = null;
    await resumed.run();
    expect(resumed.state).toBe('done');
    open();
    await tick(10);
    expect(resumed.sent).toBe(80);
  });

  it('discards an interrupted upload: S3 drops its parts and it is forgotten', async () => {
    const s3 = new FakeS3();
    const first = new Uploads(s3);
    const u = await first.start(file(80), 'uploads/gone.fq.gz');
    u.pause();
    await tick(20);
    await closeTab(first);
    const tab = new Uploads(s3);
    await tab.restore();
    expect(tab.interrupted).toHaveLength(1);
    await tab.discard(tab.interrupted[0]);
    expect(s3.aborted).toEqual([u.saved.upload_id]);
    expect(loadSaved()).toEqual([]);
    await tab.restore();
    expect(tab.interrupted).toEqual([]);
  });

  it('cancels: S3 drops the parts and nothing is left to resume', async () => {
    const s3 = new FakeS3();
    const uploads = new Uploads(s3);
    const u = await uploads.start(file(80), 'uploads/d.fq.gz');
    await u.cancel();
    expect(u.state).toBe('cancelled');
    expect(s3.aborted).toEqual([u.saved.upload_id]);
    expect(loadSaved()).toEqual([]);
    uploads.clearFinished();
    expect(uploads.list).toEqual([]);
  });

  it('never replaces an object: a taken key is refused at start, or cancels at completion', async () => {
    const s3 = new FakeS3();
    const uploads = new Uploads(s3);
    s3.taken.add('uploads/taken.fq.gz');
    await expect(uploads.start(file(8), 'uploads/taken.fq.gz')).rejects.toBeInstanceOf(KeyTaken);
    expect(uploads.list).toEqual([]);
    expect(loadSaved()).toEqual([]);

    // Another file reaches the key while this one uploads.
    const u = await uploads.start(file(80), 'uploads/race.fq.gz');
    s3.taken.add('uploads/race.fq.gz');
    await finished(u);
    expect(u.state).toBe('failed');
    expect(u.error).toMatch(/nothing was replaced/);
    expect(s3.completed.size).toBe(0);
    expect(loadSaved()).toEqual([]);
    // Failed for good: the form says so instead of "still uploading", and Retry does nothing.
    expect(u.lost).toBe(true);
    expect(u.finished).toBe(true);
    expect(uploads.pendingFor('s3://proj/uploads/race.fq.gz')).toEqual({
      name: 'P01_R1.fastq.gz',
      stage: 'lost',
      percent: null,
    });
    const puts = s3.puts.length;
    await u.run();
    expect(u.state).toBe('failed');
    expect(s3.puts.length).toBe(puts);
    uploads.clearFinished();
    expect(uploads.list).toEqual([]);
  });

  it('a failed upload is reported as failed, not as still uploading', async () => {
    const s3 = new FakeS3();
    s3.failures.set(
      1,
      Array.from({ length: 4 }, () => new Error('network down')),
    );
    const uploads = new Uploads(s3);
    const u = await startedUpload(s3, 8);
    uploads.list = [u];
    await u.run();
    const pending = uploads.pendingFor('s3://proj/uploads/x')!;
    expect(pending.stage).toBe('failed');
    expect(pendingProblem(pending)).toBe('P01_R1.fastq.gz failed to upload; retry it in Uploads');
  });

  it("words a file field's note on its upload", () => {
    const name = 'a.fq.gz';
    expect(pendingNote({ name, stage: 'uploading', percent: 42 })).toBe('Uploading a.fq.gz… 42%');
    expect(pendingNote({ name, stage: 'interrupted', percent: null })).toBe(
      'Waiting for a.fq.gz to be chosen again (see Uploads)',
    );
    // The rest say what stops the launch.
    expect(pendingNote({ name, stage: 'paused', percent: 10 })).toBe(
      'a.fq.gz is paused at 10%; resume it in Uploads',
    );
  });

  it('a re-upload to a URI supersedes an older lost or failed upload there', async () => {
    const s3 = new FakeS3();
    const uploads = new Uploads(s3);
    s3.failures.set(
      1,
      Array.from({ length: 4 }, () => new Error('network down')),
    );
    const old = await startedUpload(s3, 8);
    uploads.list = [old];
    await old.run();
    old.lost = true;
    const uri = 's3://proj/uploads/x';
    expect(uploads.pendingFor(uri)!.stage).toBe('lost');
    // The same file, uploaded again to the same key.
    let open!: () => void;
    const gate = new Promise<void>((r) => (open = r));
    const again = await startedUpload(s3, 8);
    uploads.list = [old, again];
    const putPart = s3.putPart.bind(s3);
    s3.putPart = async (...args) => (await gate, putPart(...args));
    const running = again.run();
    expect(uploads.pendingFor(uri)!.stage).toBe('uploading');
    open();
    await running;
    expect(again.state).toBe('done');
    expect(uploads.pendingFor(uri)).toBeNull();
  });

  it('a retried completion whose first answer was lost is done, not lost', async () => {
    const s3 = new FakeS3();
    const uploads = new Uploads(s3);
    s3.loseCompleteAnswer = true;
    const u = await uploads.start(file(8), 'uploads/c.fq.gz');
    await finished(u);
    expect(u.state).toBe('failed');
    // Retry: the backend no longer has the upload, because it completed.
    await u.run();
    expect(u.state).toBe('done');
    expect(u.lost).toBe(false);
    expect(u.sent).toBe(8);
    expect(loadSaved()).toEqual([]);
    expect(uploads.pendingFor('s3://proj/uploads/c.fq.gz')).toBeNull();
  });

  it('an upload a reload interrupted after it completed resumes as done', async () => {
    const s3 = new FakeS3();
    const u = await new Uploads(s3).start(file(8), 'uploads/r.fq.gz');
    await finished(u);
    // The tab went away before it forgot the upload: its record is still there.
    const reloaded = new Uploads(s3);
    reloaded.interrupted = [u.saved];
    await reloaded.resumeWith(u.saved, file(8));
    await tick(10);
    expect(reloaded.list[0].state).toBe('done');
    expect(reloaded.list[0].lost).toBe(false);
  });

  it('gives up at once on an upload S3 no longer has, or a session that ended', async () => {
    const s3 = new FakeS3();
    s3.failures.set(
      1,
      Array.from({ length: 4 }, () => new UploadGone('NoSuchUpload')),
    );
    const u = await startedUpload(s3, 8);
    await u.run();
    expect(u.lost).toBe(true);
    expect(s3.failures.get(1)).toHaveLength(3);

    const s3b = new FakeS3();
    s3b.partUrls = async () => {
      s3b.urlRequests.push([]);
      throw new AuthRequiredError();
    };
    const v = await startedUpload(s3b, 8);
    await v.run();
    expect(v.state).toBe('failed');
    expect(s3b.urlRequests).toHaveLength(1);
  });

  it('an expired upload says to upload again', async () => {
    const s3 = new FakeS3();
    const uploads = new Uploads(s3);
    const u = await uploads.start(file(8), 'uploads/e.fq.gz');
    await finished(u);
    const reloaded = new Uploads(s3);
    // Nothing landed at its key.
    const saved = {
      ...u.saved,
      upload_id: 'gone',
      key: 'uploads/f.fq.gz',
      uri: 's3://proj/uploads/f.fq.gz',
    };
    reloaded.interrupted = [saved];
    await reloaded.resumeWith(saved, file(8));
    await tick(10);
    expect(reloaded.list[0].state).toBe('failed');
    expect(reloaded.list[0].error).toMatch(/expired/);
    expect(reloaded.list[0].lost).toBe(true);
    // Forgotten: a reload does not offer it again.
    expect(loadSaved()).toEqual([]);
  });
});

/** An upload of a `size`-byte file to `uploads/x`, started on `s3` and not yet run. */
async function startedUpload(
  s3: FakeS3,
  size: number,
  wait: ConstructorParameters<typeof Upload>[3] = () => 1,
) {
  const started = await s3.start('uploads/x', size);
  const saved = { ...started, name: 'P01_R1.fastq.gz', size, last_modified: 1234 };
  return new Upload(saved, file(size), s3, wait);
}
