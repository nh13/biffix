<script lang="ts">
  // The file picker: a modal over the data sources an admin registered plus
  // the project bucket. One folder at a time with breadcrumbs, a "starts with" filter, a search
  // under the folder, a file-type filter from the field's suffixes, and a paste-a-path box. Picks a
  // file, several files, or a folder, and hands back `s3://` URIs. It browses and uploads into one
  // project: the enclosing launch form's (its version's, set by `setDataProject`), else the
  // default.
  import { tick, untrack } from 'svelte';
  import { listDataSources, type DataFile, type DataSource } from '#lib/api/index.js';
  import { FolderListing } from '#lib/data/folderListing.svelte.js';
  import { hasDataFolders } from '#lib/data/browse.js';
  import { breadcrumbs, folderOf } from '#lib/data/paths.js';
  import { errorMessage, fmtBytes, fmtRelativeMicros, plural } from '#lib/util/format.js';
  import { isS3Uri, locate, matchesSuffix } from './picker';
  import { getDataProject } from '#lib/data/projectContext.js';
  import { defaultUploadFolder } from '#lib/upload/keys.js';
  import type { Upload } from '#lib/upload/uploads.svelte.js';
  import UploadPanel from '#lib/upload/UploadPanel.svelte';
  import Dialog from '#lib/components/Dialog.svelte';

  interface Props {
    /** `file`: one object; `files`: several; `folder`: a folder. */
    mode: 'file' | 'files' | 'folder';
    /** What is being picked, for the title ("Samplesheet"). */
    label: string;
    /** File-name endings the field accepts; the listing filters to them (switchable). */
    suffixes?: string[];
    /** The field's current value: the picker opens at its folder. */
    current?: string | null;
    onpick: (uris: string[]) => void;
    onclose: () => void;
  }
  let { mode, label, suffixes = [], current = null, onpick, onclose }: Props = $props();
  // The project to browse and upload into: the enclosing launch form's, else the default.
  const contextProject = getDataProject();
  const project = $derived(contextProject());

  let sources = $state<DataSource[]>([]);
  // The folder on show (the same class the Data page browses with).
  const listing = new FolderListing();
  listing.loading = true;
  let onlyMatching = $state(true);
  let selected = $state<Map<string, DataFile>>(new Map());
  let pasted = $state('');
  // No data folder is connected (an admin's job, in the deploy config): said as such, not as an
  // error to retry. The project's own storage, always listed, stays browsable; with no source at
  // all, there is nothing to list.
  let noDataFolders = $state(false);
  let noSources = $state(false);

  $effect(() => {
    // The project is read once: the dialog is opened afresh for each pick.
    listDataSources(untrack(() => project))
      .then((all) => {
        sources = all;
        noDataFolders = !hasDataFolders(all);
        const at = current ? locate(all, current) : null;
        if (at) listing.open(at.source, at.folder, project);
        else if (all.length > 0) listing.open(all[0], '', project);
        else {
          listing.loading = false;
          noSources = true;
        }
      })
      .catch((e) => {
        listing.loading = false;
        listing.error = errorMessage(e);
      });
  });

  const listed = $derived(listing.searchResults ?? listing.files);
  const shownFiles = $derived(
    listed.filter((f) => !onlyMatching || matchesSuffix(f.name, suffixes)),
  );
  // Files folder mode never lists are not hidden by a filter.
  const hiddenCount = $derived(mode === 'folder' ? 0 : listed.length - shownFiles.length);
  const crumbs = $derived(listing.source ? breadcrumbs(listing.source.name, listing.path) : []);

  function toggle(f: DataFile) {
    const next = new Map(mode === 'files' ? selected : []);
    if (selected.has(f.uri)) next.delete(f.uri);
    else next.set(f.uri, f);
    selected = next;
    pasted = '';
  }
  function selectAllShown() {
    const next = new Map(selected);
    for (const f of shownFiles) next.set(f.uri, f);
    selected = next;
    pasted = '';
  }
  const pastedOk = $derived(isS3Uri(pasted));
  const folderUri = $derived(listing.source ? `${listing.source.uri}${listing.path}` : '');
  const canUse = $derived(pastedOk || (mode === 'folder' ? !!listing.source : selected.size > 0));
  const useLabel = $derived(
    pastedOk
      ? 'Use this path'
      : mode === 'folder'
        ? 'Use this folder'
        : mode === 'files' && selected.size > 1
          ? `Use ${selected.size} files`
          : 'Use this file',
  );
  const selectedBytes = $derived([...selected.values()].reduce((n, f) => n + f.size, 0));

  function use() {
    if (pastedOk) onpick([pasted.trim()]);
    else if (mode === 'folder') onpick([folderUri]);
    else onpick([...selected.keys()]);
    onclose();
  }

  // -- Upload (the project bucket only, when that project allows it) ------------------------------
  let uploadOpen = $state(false);
  let uploadFolder = $state('');
  // Each Upload files… opens the panel afresh (its folder from where the picker is now).
  let uploadSeq = $state(0);
  let dragging = $state(false);
  let panel = $state<ReturnType<typeof UploadPanel> | null>(null);
  const canUpload = $derived(!!listing.source?.writable);
  function openUpload() {
    uploadFolder = defaultUploadFolder(listing.path, new Date());
    uploadSeq++;
    uploadOpen = true;
  }
  /** Started uploads, handed back where they will be, as if they had been picked. */
  function uploaded(started: Upload[], keys: string[]) {
    const source = listing.source;
    if (!source) return;
    onpick(mode === 'folder' ? [`${source.uri}${folderOf(keys[0])}`] : started.map((u) => u.uri));
    onclose();
  }
  async function onDrop(e: DragEvent) {
    dragging = false;
    if (!canUpload || !e.dataTransfer?.files.length) return;
    e.preventDefault();
    const files = [...e.dataTransfer.files];
    if (!uploadOpen) {
      openUpload();
      await tick();
    }
    void panel?.addFiles(files);
  }
</script>

<Dialog labelledby="picker-title" {onclose} size="picker" closeOnBackdrop={false}>
  <header>
    <h2 id="picker-title">
      Choose {mode === 'folder' ? 'a folder' : mode === 'files' ? 'files' : 'a file'} for {label}
    </h2>
    <button class="icon" aria-label="Close" onclick={onclose}>×</button>
  </header>

  <div class="body">
    <nav class="sources" aria-label="Data sources">
      {#each sources as s (s.id)}
        <button
          class:active={listing.source?.id === s.id}
          aria-current={listing.source?.id === s.id ? 'true' : undefined}
          onclick={() => listing.open(s, '', project)}
          title={s.uri}
        >
          <span class="src-name">{s.name}</span>
          {#if s.kind === 'references'}<span class="src-kind">references</span>{/if}
        </button>
      {/each}
    </nav>
    <select
      class="sources-narrow"
      aria-label="Data source"
      value={listing.source?.id ?? ''}
      onchange={(e) => {
        const s = sources.find((x) => x.id === (e.currentTarget as HTMLSelectElement).value);
        if (s) listing.open(s, '', project);
      }}
    >
      {#each sources as s (s.id)}<option value={s.id}>{s.name}</option>{/each}
    </select>

    <section class="listing" aria-label="Folder contents">
      <nav class="crumbs" aria-label="Folder path">
        {#each crumbs as c, i (c.path)}
          {#if i > 0}<span class="sep" aria-hidden="true">/</span>{/if}
          {#if i === crumbs.length - 1}<span aria-current="page">{c.label}</span>
          {:else}<button
              class="link"
              onclick={() => listing.source && listing.open(listing.source, c.path, project)}
              >{c.label}</button
            >{/if}
        {/each}
      </nav>

      <div class="tools">
        <input
          type="search"
          placeholder="Name starts with…"
          aria-label="Filter this folder by name"
          bind:value={listing.startsWith}
          oninput={() => listing.filterChanged()}
          onkeydown={(e) => e.key === 'Enter' && mode !== 'folder' && listing.search()}
        />
        <!-- A search finds files only: no use when a folder is picked. -->
        {#if mode !== 'folder'}
          <button
            class="btn btn-small"
            onclick={() => listing.search()}
            disabled={!listing.startsWith.trim()}>Search subfolders</button
          >
        {/if}
        {#if mode === 'files' && shownFiles.length > 0}
          <button class="btn btn-small" onclick={selectAllShown}
            >Select all {shownFiles.length}</button
          >
        {/if}
        {#if canUpload}
          <button class="btn btn-small" onclick={openUpload} aria-expanded={uploadOpen}
            >Upload files…</button
          >
        {/if}
        {#if suffixes.length > 0 && mode !== 'folder'}
          <label class="only">
            <input type="checkbox" bind:checked={onlyMatching} />
            Only {suffixes.join(', ')}
          </label>
        {/if}
      </div>

      {#if uploadOpen && listing.source}
        {#key uploadSeq}<UploadPanel
            bind:this={panel}
            source={listing.source}
            folder={uploadFolder}
            {project}
            multiple={mode !== 'file'}
            onstarted={uploaded}
          />{/key}
      {/if}

      {#if noDataFolders}
        <p class="no-sources">
          No data folders are connected — an admin adds them in the deploy config.
          {#if noSources}You can still paste an <code>s3://</code> path.{:else}Your project's own
            storage is below, and you can still paste an <code>s3://</code> path.{/if}
        </p>
      {/if}
      {#if listing.error}
        <div class="error" role="alert">
          {listing.error}
          <button class="link" onclick={() => listing.retry()}>Retry</button>
        </div>
      {/if}

      <div
        class="rows"
        class:dragging
        aria-busy={listing.loading}
        role="presentation"
        ondragover={(e) => {
          if (canUpload && e.dataTransfer?.types.includes('Files')) {
            e.preventDefault();
            dragging = true;
          }
        }}
        ondragleave={() => (dragging = false)}
        ondrop={onDrop}
      >
        {#if listing.searchResults}
          <p class="muted">
            {plural(listing.searchResults.length, 'match', 'matches')} under this folder{listing.searchTruncated
              ? ' (stopped early — narrow the search)'
              : ''}.
            <button class="link" onclick={() => (listing.searchResults = null)}
              >Back to the folder</button
            >
          </p>
        {:else}
          {#each listing.folders as f (f.path)}
            <button
              class="row folder"
              onclick={() => listing.source && listing.open(listing.source, f.path, project)}
            >
              <span class="glyph" aria-hidden="true">📁</span>
              <span class="name">{f.name}</span>
              <span class="meta">folder</span>
            </button>
          {/each}
        {/if}
        {#if mode !== 'folder'}
          {#each shownFiles as f (f.uri)}
            <label class="row file" class:checked={selected.has(f.uri)}>
              <input
                type={mode === 'files' ? 'checkbox' : 'radio'}
                name="picker-file"
                checked={selected.has(f.uri)}
                onchange={() => toggle(f)}
              />
              <span class="name" title={f.uri}>{listing.searchResults ? f.path : f.name}</span>
              <span class="meta">{fmtBytes(f.size)}</span>
              <span class="meta when">{fmtRelativeMicros(f.last_modified_micros)}</span>
            </label>
          {/each}
        {/if}
        {#if listing.loading}
          <p class="muted">Loading…</p>
        {:else if !listing.error && !noSources && listing.folders.length === 0 && shownFiles.length === 0 && !listing.searchResults}
          <p class="muted">
            {hiddenCount > 0
              ? `Nothing here matches ${suffixes.join(', ')} (${plural(hiddenCount, 'other file')}).`
              : 'Nothing here yet.'}
          </p>
        {/if}
        {#if hiddenCount > 0 && shownFiles.length > 0}
          <p class="muted small">
            {plural(hiddenCount, 'other file')} hidden by the file-type filter.
          </p>
        {/if}
        {#if listing.cursor && !listing.searchResults && !listing.loading}
          <button class="btn btn-small" onclick={() => listing.load(true)}>Show more</button>
        {/if}
      </div>
    </section>
  </div>

  <footer>
    <div class="selection">
      {#if mode === 'folder'}
        <span class="mono">{folderUri}</span>
      {:else if selected.size > 0}
        <span>{selected.size} selected · {fmtBytes(selectedBytes)}</span>
      {/if}
      <input
        class="paste"
        type="text"
        spellcheck="false"
        placeholder="or paste an s3:// path"
        aria-label="Paste an s3 path"
        bind:value={pasted}
      />
    </div>
    <div class="actions">
      <button class="btn btn-compact" onclick={onclose}>Cancel</button>
      <button class="btn btn-compact btn-primary" onclick={use} disabled={!canUse}
        >{useLabel}</button
      >
    </div>
  </footer>
</Dialog>

<style>
  header,
  footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.75rem;
    padding: 0.7rem 1rem;
    border-bottom: 1px solid var(--card-border);
  }
  footer {
    border-bottom: none;
    border-top: 1px solid var(--card-border);
    flex-wrap: wrap;
  }
  h2 {
    margin: 0;
    font-size: 1rem;
  }
  .icon {
    border: none;
    background: none;
    color: var(--text-muted);
    font-size: 1.3rem;
    cursor: pointer;
  }
  .body {
    flex: 1;
    min-height: 0;
    display: grid;
    grid-template-columns: 13rem 1fr;
  }
  .sources {
    border-right: 1px solid var(--card-border);
    padding: 0.5rem;
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    overflow-y: auto;
  }
  .sources button {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    text-align: left;
    padding: 0.45rem 0.6rem;
    border: none;
    border-radius: 0.4rem;
    background: none;
    color: var(--text);
    cursor: pointer;
    font: inherit;
    font-size: 0.85rem;
  }
  .sources button:hover {
    background: var(--hover);
  }
  .sources button.active {
    background: var(--active);
    font-weight: 600;
  }
  .src-kind {
    font-size: 0.7rem;
    color: var(--text-muted);
    font-weight: 400;
  }
  .sources-narrow {
    display: none;
  }
  .listing {
    display: flex;
    flex-direction: column;
    min-height: 0;
    padding: 0.6rem 1rem;
  }
  .crumbs {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    flex-wrap: wrap;
    font-size: 0.85rem;
    margin-bottom: 0.5rem;
  }
  .sep {
    color: var(--text-muted);
  }
  .rows.dragging {
    outline: 2px dashed var(--accent);
    outline-offset: -2px;
  }
  .tools {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
    margin-bottom: 0.5rem;
  }
  .tools input[type='search'],
  .paste {
    font: inherit;
    font-size: 0.82rem;
    padding: 0.3rem 0.5rem;
    border: 1px solid var(--control-border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text);
    min-width: 12rem;
  }
  .paste {
    font-family: var(--mono);
    width: min(24rem, 100%);
  }
  .only {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    font-size: 0.8rem;
    color: var(--text-muted);
  }
  .rows {
    flex: 1;
    overflow-y: auto;
    border: 1px solid var(--card-border);
    border-radius: 0.45rem;
  }
  .row {
    display: grid;
    grid-template-columns: 1.5rem 1fr auto auto;
    align-items: center;
    gap: 0.6rem;
    width: 100%;
    box-sizing: border-box;
    padding: 0.4rem 0.7rem;
    border: none;
    border-bottom: 1px solid var(--card-border);
    background: none;
    color: var(--text);
    font: inherit;
    font-size: 0.83rem;
    text-align: left;
    cursor: pointer;
  }
  .row:hover {
    background: var(--hover);
  }
  .row.checked {
    background: var(--active);
  }
  .row .name {
    font-family: var(--mono);
    font-size: 0.8rem;
    overflow-wrap: anywhere;
  }
  .meta {
    font-size: 0.75rem;
    color: var(--text-muted);
    white-space: nowrap;
  }
  .selection {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    flex-wrap: wrap;
    font-size: 0.82rem;
    min-width: 0;
  }
  .mono {
    font-size: 0.78rem;
    overflow-wrap: anywhere;
  }
  .btn-small {
    margin: 0.4rem;
  }
  .tools .btn-small {
    margin: 0;
  }
  button:focus-visible,
  input:focus-visible,
  select:focus-visible,
  .row:focus-within {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .error {
    font-size: 0.82rem;
    margin-bottom: 0.5rem;
  }
  .no-sources {
    margin: 0.5rem 0;
    color: var(--text-muted);
    font-size: 0.85rem;
  }
  .muted {
    font-size: 0.82rem;
    padding: 0.5rem 0.7rem;
    margin: 0;
  }
  .small {
    font-size: 0.76rem;
  }
  @media (max-width: 800px) {
    .body {
      grid-template-columns: 1fr;
      grid-template-rows: auto 1fr;
    }
    .sources {
      display: none;
    }
    .sources-narrow {
      display: block;
      margin: 0.5rem 1rem 0;
      font: inherit;
      padding: 0.35rem;
      border: 1px solid var(--control-border);
      border-radius: 0.4rem;
      background: var(--bg-content);
      color: var(--text);
    }
    .when {
      display: none;
    }
  }
</style>
