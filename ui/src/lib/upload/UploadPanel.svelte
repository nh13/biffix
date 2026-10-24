<script lang="ts">
  // Upload files into a writable data source (the project bucket, when its project allows it): the
  // folder to upload to (under `uploads/`, the only place the backend takes uploads), a file chooser
  // (or files dropped on the caller's area, through `addFiles`), and another name for a file whose
  // name is taken (an upload never replaces an object). The uploads themselves run in the shared
  // engine and its tray, so they keep going while the person works; `onstarted` hears each batch
  // once all of it has started (less any file the person chose not to upload when its name was
  // taken, or went on without when it could not start; never an empty batch). The file picker and the Lab Data page both upload through this.
  import type { DataSource } from '#lib/api/index.js';
  import { folderOf } from '#lib/data/paths.js';
  import { renamedKey, uploadKey } from './keys';
  import { uploads } from './store';
  import { KeyTaken, type Upload } from './uploads.svelte';
  import { errorMessage } from '#lib/util/format.js';

  interface Props {
    /** The source uploaded into (its root is shown before the folder). */
    source: DataSource;
    /** The folder, relative to the source root, filled in at first. */
    folder: string;
    /** The project whose bucket it is (`null`: the default project). */
    project?: string | null;
    /** Several files at once, or only one. */
    multiple?: boolean;
    /** Started: every file of the batch, and the key each went to. */
    onstarted: (started: Upload[], keys: string[]) => void;
  }
  let {
    source,
    folder: initialFolder,
    project = null,
    multiple = true,
    onstarted,
  }: Props = $props();

  // Filled in once when shown; the person may change it.
  // svelte-ignore state_referenced_locally
  let folder = $state(initialFolder);
  let error = $state<string | null>(null);
  let starting = $state(false);
  let fileInput = $state<HTMLInputElement | null>(null);
  /**
   * A file whose name is taken in the folder: nothing of it was sent, and another name is offered.
   * `files`/`keys`/`started` resume the batch it stopped.
   */
  let taken = $state<{ message: string; name: string } | null>(null);
  let takenBatch: Batch | null = null;
  /**
   * A batch that stopped part-way on another error: the files before it started (and keep
   * uploading), so the person tries the rest again or goes on with those alone.
   */
  let stopped = $state<{ message: string; batch: Batch } | null>(null);
  /** Files of the latest batch the person chose not to upload (a name was taken). */
  let skipped = $state<string[]>([]);

  /** A batch being started: its files, the key each goes to, and the uploads started so far. */
  interface Batch {
    files: File[];
    keys: string[];
    /** Each file's key as first given, which a name offered for a taken key is built from. */
    asked: string[];
    started: Upload[];
    /** The `-<n>` the next name offered for a taken key gets. */
    rename: number;
  }

  /** Upload `files` into the folder (only the first when one file is wanted). */
  export async function addFiles(files: File[]) {
    if (files.length === 0) return;
    // Files dropped while a batch is still starting would race it.
    if (starting || taken || stopped) {
      error = 'Finish with the files already added, then add these again.';
      return;
    }
    files = multiple ? [...files] : files.slice(0, 1);
    skipped = [];
    const keys: string[] = [];
    for (const f of files) {
      const k = uploadKey(folder, f.name);
      if ('error' in k) {
        error = k.error;
        return;
      }
      keys.push(k.key);
    }
    await startUploads({ files, keys, asked: [...keys], started: [], rename: 2 });
  }

  /** Start the batch's uploads after the ones already started, then report them all. */
  async function startUploads(batch: Batch) {
    const { files, keys, started } = batch;
    starting = true;
    error = null;
    taken = null;
    stopped = null;
    try {
      for (let i = started.length; i < files.length; i++) {
        started.push(await uploads.start(files[i], keys[i], project));
        batch.rename = 2;
      }
      report(batch);
    } catch (e) {
      const i = started.length;
      if (e instanceof KeyTaken) {
        // Offered `-2`, then `-3` if that is taken too.
        const key = renamedKey(batch.asked[i], batch.rename++);
        taken = { message: e.message, name: key.slice(folderOf(key).length) };
        takenBatch = batch;
      } else if (i === 0) error = errorMessage(e);
      else {
        const message = `${files[i].name} could not start: ${errorMessage(e)}. ${i} of ${files.length} files started and keep uploading.`;
        stopped = { message, batch };
      }
    } finally {
      starting = false;
    }
  }

  /** Report what of the batch started (nothing, when its one file was skipped). */
  function report(batch: Batch) {
    const n = batch.started.length;
    if (n > 0) onstarted(batch.started, batch.keys.slice(0, n));
  }

  /** Upload the file whose name was taken under the new name, then the rest of its batch. */
  function uploadRenamed() {
    if (!taken || !takenBatch) return;
    const batch = takenBatch;
    const i = batch.started.length;
    const k = uploadKey(folderOf(batch.keys[i]), taken.name.trim());
    if ('error' in k) {
      error = k.error;
      return;
    }
    batch.keys[i] = k.key;
    takenBatch = null;
    void startUploads(batch);
  }
  /** Leave out only the file whose name was taken, and go on with the rest of its batch. */
  function skipTaken() {
    if (!taken || !takenBatch) return;
    const batch = takenBatch;
    const i = batch.started.length;
    skipped = [...skipped, batch.files[i].name];
    batch.files.splice(i, 1);
    batch.keys.splice(i, 1);
    batch.asked.splice(i, 1);
    takenBatch = null;
    taken = null;
    batch.rename = 2;
    void startUploads(batch);
  }
  /** Go on with the files of a stopped batch that started, leaving out the rest. */
  function goOnWithout() {
    if (!stopped) return;
    const { batch } = stopped;
    stopped = null;
    skipped = batch.files.slice(batch.started.length).map((f) => f.name);
    report(batch);
  }
</script>

<div class="upload" role="group" aria-label="Upload files">
  <label class="upload-to">
    <span>Upload to</span>
    <span class="mono root">{source.uri}</span>
    <input class="mono" bind:value={folder} aria-label="Upload folder" spellcheck="false" />
  </label>
  <div class="upload-actions">
    <button class="btn btn-small btn-primary" disabled={starting} onclick={() => fileInput?.click()}
      >{starting ? 'Starting…' : multiple ? 'Choose files…' : 'Choose a file…'}</button
    >
    <span class="muted small">or drop files here. They keep uploading while you work.</span>
    <input
      bind:this={fileInput}
      type="file"
      class="hidden-input"
      {multiple}
      aria-label="Files to upload"
      onchange={(e) => {
        const input = e.currentTarget as HTMLInputElement;
        const chosen = [...(input.files ?? [])];
        input.value = '';
        addFiles(chosen);
      }}
    />
  </div>
  {#if error}<p class="upload-error" role="alert">{error}</p>{/if}
  {#if skipped.length > 0}<p class="muted small" role="status" data-testid="upload-skipped">
      Not uploaded: {skipped.join(', ')}
    </p>{/if}
  {#if stopped}
    <div class="upload-taken" role="group" aria-label="Upload stopped">
      <p class="upload-error" role="alert">{stopped.message}</p>
      <div class="upload-actions">
        <button
          class="btn btn-small btn-primary"
          disabled={starting}
          onclick={() => stopped && startUploads(stopped.batch)}>Try the rest again</button
        >
        <button class="btn btn-small" disabled={starting} onclick={goOnWithout}
          >Go on without the rest</button
        >
      </div>
    </div>
  {/if}
  {#if taken}
    <div class="upload-taken" role="group" aria-label="Name taken">
      <p class="upload-error" role="alert">{taken.message}</p>
      <label class="upload-to">
        <span>Upload as</span>
        <input class="mono" bind:value={taken.name} aria-label="New file name" spellcheck="false" />
      </label>
      <div class="upload-actions">
        <button class="btn btn-small btn-primary" disabled={starting} onclick={uploadRenamed}
          >Upload under this name</button
        >
        <button class="btn btn-small" disabled={starting} onclick={skipTaken}
          >Don't upload it</button
        >
      </div>
    </div>
  {/if}
</div>

<style>
  .upload {
    display: grid;
    gap: 0.4rem;
    padding: 0.6rem 0.7rem;
    margin-bottom: 0.5rem;
    border: 1px dashed var(--control-border);
    border-radius: 0.45rem;
  }
  .upload-to {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    flex-wrap: wrap;
    font-size: 0.82rem;
  }
  .upload-to .root {
    color: var(--text-muted);
    font-size: 0.78rem;
  }
  .upload-to input {
    flex: 1 1 12rem;
    font: inherit;
    font-family: var(--mono);
    font-size: 0.78rem;
    padding: 0.25rem 0.45rem;
    border: 1px solid var(--control-border);
    border-radius: 0.35rem;
    background: var(--bg-content);
    color: var(--text);
  }
  .upload-actions {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    flex-wrap: wrap;
  }
  .upload-taken {
    display: grid;
    gap: 0.4rem;
  }
  .upload-error {
    margin: 0;
    font-size: 0.8rem;
    color: var(--fg-fail);
  }
  .hidden-input {
    display: none;
  }
  .mono {
    font-size: 0.78rem;
    overflow-wrap: anywhere;
  }
  .small {
    font-size: 0.76rem;
  }
  button:focus-visible,
  input:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
</style>
