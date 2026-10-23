<script lang="ts">
  // The Data browser, in the Lab or the Expert view (`presentation`): the project's bucket and its
  // data sources, one folder at a time, with paging, a name search, the file viewer for tables,
  // text, images, PDFs and HTML, and downloads. The folder and the open file are the page's URL
  // (`path`, `file`); opening or closing a file changes it. The Lab view says it in plain words,
  // names an analysis's results folder by the analysis ("Result of …", linked), never links a folder
  // biffix hides, takes uploads at the top of the project (into `uploads/`) and opens an analysis's
  // BAM/CRAM in the genome browser and its FastQC reports in the FastQC viewer. The Expert view
  // shows the bucket's own paths. The pure parts are in `dataBrowser.ts`; every word that differs
  // between the views, or that only one view says, is in `presentation.ts`.
  import { tick, untrack } from 'svelte';
  import { goto } from '$app/navigation';
  import {
    getDataDownloadUrl,
    getDataPreview,
    getDataViewUrl,
    listDataFolder,
    type DataFile,
    type DataSource,
    type Project,
  } from '#lib/api/index.js';
  import { CopiedFlag } from '#lib/util/clipboard.svelte.js';
  import { errorMessage } from '#lib/util/format.js';
  import { runNames } from '#lib/runs/runNames.svelte.js';
  import { runTabHref } from '#lib/runs/runNav.js';
  import OutputViewer from '#lib/components/OutputViewer.svelte';
  import type { OpenTarget } from '#lib/qc/open.js';
  import ResultViewer from '#lib/qc/ResultViewer.svelte';
  import { defaultUploadFolder } from '#lib/upload/keys.js';
  import UploadPanel from '#lib/upload/UploadPanel.svelte';
  import type { Upload } from '#lib/upload/uploads.svelte.js';
  import type { ViewerFile } from '#lib/components/viewerFile.js';
  import { hasDataFolders } from './browse';
  import { analysesShown, resultOpen } from './dataBrowser';
  import DataToolbar from './DataToolbar.svelte';
  import FolderTable from './FolderTable.svelte';
  import { FolderListing } from './folderListing.svelte';
  import { RUNS_FOLDER, runOfPath, topShortcuts, uploadsHere } from './labPresentation';
  import { chosenProject, dataHref, dataProjectHref, folderOf, offersProjectChoice } from './paths';
  import type { DataPresentation } from './presentation';
  import { runOutputAt } from './runOutput';

  interface Props {
    presentation: DataPresentation;
    /** The deploy's projects (`null`: not loaded, browsed as a deploy of one). */
    projects: Project[] | null;
    /** The project the URL names (`null`: the default one). */
    project: string | null;
    /** The project's sources (`null`: loading). */
    sources: DataSource[] | null;
    sourcesError: string | null;
    /** The source the URL names (`null`: none, the first one). */
    sourceId: string | null;
    /** The folder on show, relative to the source's root. */
    path: string;
    /** The file the URL names: a name in the folder, or (opened from a search) a path below it. */
    file: string | null;
  }
  let {
    presentation: p,
    projects,
    project,
    sources,
    sourcesError,
    sourceId,
    path,
    file,
  }: Props = $props();

  // The project the selector shows as chosen; choosing the default one drops it from the URL.
  const chosen = $derived(chosenProject(project, projects));
  function chooseProject(id: string) {
    const isDefault = projects?.find((pr) => pr.id === id)?.default ?? false;
    goto(dataProjectHref(isDefault ? null : id));
  }
  // The source on show, and whether the URL names one this project does not have.
  const source = $derived(
    sources?.find((s) => s.id === sourceId) ?? (sourceId === null ? (sources?.[0] ?? null) : null),
  );
  const unknownSource = $derived(!!sources && sourceId !== null && !source);

  const listing = new FolderListing();

  // Follow the URL: list the folder it names (and only when it changes). A failed download in the
  // folder left behind is not this folder's.
  $effect(() => {
    const s = source;
    const at = path;
    const proj = project;
    if (s && (listing.source?.id !== s.id || listing.path !== at || listing.project !== proj)) {
      listing.open(s, at, proj);
      downloadError = null;
    }
  });

  const shownFiles = $derived(listing.searchResults ?? listing.files);
  const searching = $derived(!!listing.searchResults);
  const nameOf = (id: string) => runNames.name(id);
  const crumbs = $derived(source ? p.trail(source, path, nameOf) : []);

  // -- Viewing and downloading --------------------------------------------------------------------
  // The file the URL names, from the source root.
  const viewingPath = $derived(file ? `${path}${file}` : null);
  // The file among what the page has loaded: the folder's pages so far, or the search results.
  const listed = $derived.by<DataFile | null>(() => {
    const full = viewingPath;
    if (full === null) return null;
    return (
      listing.files.find((f) => f.path === full) ??
      listing.searchResults?.find((f) => f.path === full) ??
      null
    );
  });
  // A linked file the page hasn't loaded (past the pages listed so far, or a search hit after a
  // reload or from a shared link) is looked up on its own: its folder, listed by that name as a
  // prefix (the exact name sorts before every longer one, so it is on the first page). The answer is
  // kept with the source and path it was for, so it is never shown for another link.
  const lookupKey = $derived(
    source && viewingPath !== null ? `${project ?? ''}\n${source.id}\n${viewingPath}` : null,
  );
  let lookedUp = $state<{ key: string; file: DataFile | null; error: string | null } | null>(null);
  $effect(() => {
    const key = lookupKey;
    const s = source;
    const full = viewingPath;
    // Wait for the listing, which usually has the file.
    if (!key || !s || full === null || listed || listing.loading) return;
    if (untrack(() => lookedUp?.key) === key) return;
    const folder = folderOf(full);
    const name = full.slice(folder.length);
    let cancelled = false;
    listDataFolder(s.id, folder, { startsWith: name, project })
      .then((found) => {
        if (!cancelled)
          lookedUp = { key, file: found.files.find((f) => f.path === full) ?? null, error: null };
      })
      .catch((e) => {
        if (!cancelled) lookedUp = { key, file: null, error: errorMessage(e) };
      });
    return () => {
      cancelled = true;
    };
  });
  const lookup = $derived(lookedUp && lookedUp.key === lookupKey ? lookedUp : null);
  const viewing = $derived<DataFile | null>(listed ?? lookup?.file ?? null);
  // The link names a file that isn't there (or couldn't be looked up): say so, not nothing.
  const missing = $derived(!listed && lookup && !lookup.file ? lookup : null);
  function show(f: DataFile | null) {
    if (!source) return;
    const href = dataHref(source.id, path, f ? f.path.slice(path.length) : undefined, project);
    goto(href, { replace: !f, reset: false });
  }

  let downloadError = $state<string | null>(null);
  async function download(f: DataFile) {
    if (!source) return;
    downloadError = null;
    // The folder the download was asked from: a failure that answers after the page has moved
    // on (another folder, source or project) is not the new place's.
    const from = { source, path, project };
    try {
      window.location.assign((await getDataDownloadUrl(source.id, f.path, project)).url);
    } catch (e) {
      if (from.source === source && from.path === path && from.project === project)
        downloadError = `Couldn't download ${f.name}: ${errorMessage(e)}`;
    }
  }
  function viewerFile(s: DataSource, f: DataFile): ViewerFile {
    const proj = project;
    return {
      key: `${proj ?? ''}:${s.id}:${f.path}`,
      name: f.name,
      path: f.uri,
      size: f.size,
      viewer: f.viewer,
      preview: () => getDataPreview(s.id, f.path, proj),
      viewUrl: () => getDataViewUrl(s.id, f.path, proj),
      download: () => download(f),
    };
  }

  // -- Analyses: named, and their BAM/CRAM and FastQC reports opened as their results -------------
  const run = $derived(source && p.analyses ? runOfPath(source, path) : null);
  const shortcuts = $derived(source && p.analyses ? topShortcuts(source, path) : []);
  // Name the analyses this folder shows: its own, its run folders', and those search results are in.
  $effect(() => {
    const s = source;
    if (!s || !p.analyses) return;
    runNames.want(analysesShown(s, path, listing.folders, listing.searchResults));
  });

  let open = $state<OpenTarget | null>(null);
  let openError = $state<string | null>(null);
  let opening = $state<string | null>(null);
  /**
   * Bumped whenever the place changes (the source, folder, project or view), and by each open: an
   * answer for a place left behind, or for an open since superseded, is dropped, so it never opens,
   * or explains, a file somewhere else.
   */
  let placeSeq = 0;
  async function openAsResult(f: DataFile, kind: 'alignment' | 'fastqc') {
    const words = p.analyses;
    if (!source || !words) return;
    const r = runOfPath(source, f.path);
    if (!r) return;
    // Each open takes its own turn: a later one (or a move elsewhere) drops this one's answer.
    const seq = ++placeSeq;
    openError = null;
    opening = f.uri;
    try {
      const found = await runOutputAt(r.runId, r.rest, f.uri);
      if (seq !== placeSeq) return;
      const next = resultOpen(found, kind, r.runId, f);
      if ('open' in next) open = next.open;
      else if ('inViewer' in next) show(f);
      else openError = words.cannotOpen[next.cannot](f.name);
    } catch (e) {
      if (seq === placeSeq) openError = `Couldn't open ${f.name}: ${errorMessage(e)}`;
    } finally {
      if (seq === placeSeq) opening = null;
    }
  }

  // -- Upload (Lab: at the top of your project, and under uploads/) -------------------------------
  let uploadOpen = $state(false);
  let uploadSeq = $state(0);
  let uploadFolder = $state('');
  let dragging = $state(false);
  let panel = $state<ReturnType<typeof UploadPanel> | null>(null);
  const canUpload = $derived(
    !!p.uploads && !!source && uploadsHere(source, path) && !listing.searchResults,
  );
  /** Where the upload form was opened: its batch goes there, wherever the page is by then. */
  let uploadPlace = $state<{ source: DataSource; project: string | null } | null>(null);
  // The batch started last, and the place it went to: said until it is done, then linked.
  let batch = $state<{
    uploads: Upload[];
    folder: string;
    source: DataSource;
    project: string | null;
  } | null>(null);
  /** The batch, and how many of its files were done, when its folder was last listed again. */
  let relisted: { batch: object; done: number } | null = null;
  const batchDone = $derived(!!batch && batch.uploads.every((u) => u.finished));
  // Nothing of the batch is moving: what is left (if anything) is paused or failed.
  const batchMoving = $derived(
    !!batch && batch.uploads.some((u) => u.state === 'uploading' || u.state === 'completing'),
  );
  const batchFilesDone = $derived(batch?.uploads.filter((u) => u.state === 'done').length ?? 0);
  function openUpload() {
    if (!source) return;
    uploadFolder = defaultUploadFolder(path, new Date());
    uploadPlace = { source, project };
    uploadSeq++;
    uploadOpen = true;
  }
  // The panel never starts an empty batch; its files all go to one folder.
  function started(list: Upload[], keys: string[]) {
    if (!uploadPlace) return;
    batch = { uploads: list, folder: folderOf(keys[0]), ...uploadPlace };
    uploadOpen = false;
  }
  // Once the batch stops moving with files done (again, after a Resume or Retry finished more),
  // list its folder again if it is on show: the new files appear in it.
  $effect(() => {
    const b = batch;
    const done = batchFilesDone;
    if (!b || batchMoving || done === 0) return;
    if (relisted?.batch === b && relisted.done === done) return;
    relisted = { batch: b, done };
    untrack(() => {
      if (listing.source?.id === b.source.id && listing.project === b.project) listing.load();
    });
  });
  // Leaving the place (another folder, source, project or view) closes the upload form (it would
  // upload into the folder it was opened at), drops a can't-open note about a file of the place
  // left, and drops a pending open there (its button is free again). Keyed by value: a source
  // listed again as a new object, or another file opened in the folder, is the same place.
  const place = $derived(`${project ?? ''}\n${source?.id ?? ''}\n${path}\n${p.view}`);
  $effect(() => {
    void place;
    untrack(() => {
      placeSeq++;
      uploadOpen = false;
      openError = null;
      opening = null;
    });
  });
  // Switching view closes an open result and forgets a *Copied* label or a drag of the view left;
  // the folder, its listing, the open file and an upload still going carry over.
  let shownView = untrack(() => p.view);
  $effect(() => {
    const v = p.view;
    if (v === shownView) return;
    shownView = v;
    untrack(() => {
      open = null;
      dragging = false;
      copied.key = null;
    });
  });
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

  const copied = new CopiedFlag();
</script>

<div class="data-browser {p.view}">
  <div class="head">
    <h1>Data</h1>
    {#if projects && offersProjectChoice(projects)}
      <label class="project">
        <span>Project</span>
        <select value={chosen} onchange={(e) => chooseProject(e.currentTarget.value)}>
          {#each projects as pr (pr.id)}
            <option value={pr.id}>{pr.id}</option>
          {/each}
          {#if chosen && !projects.some((pr) => pr.id === chosen)}
            <!-- A link to a project this deploy does not have: shown as chosen, and refused below. -->
            <option value={chosen}>{chosen}</option>
          {/if}
        </select>
      </label>
    {/if}
  </div>
  <p class="lede">{p.lede}</p>

  {#if sourcesError}
    <p class="error" role="alert">{p.sourcesError(sourcesError)}</p>
  {:else if sources === null}
    <p class="muted">Loading…</p>
  {:else if sources.length === 0}
    <p class="muted">{p.noSources}</p>
  {:else}
    {#if !hasDataFolders(sources)}
      <!-- The project's own storage is always listed: still browsable, under the note. -->
      <p class="muted">{p.noDataFolders}</p>
    {/if}
    <div class="browser">
      <nav class="sources" aria-label="Data sources">
        {#each sources as s (s.id)}
          {@const kind = p.sourceKind(s)}
          <a
            href={dataHref(s.id, '', undefined, project)}
            class:active={source?.id === s.id}
            aria-current={source?.id === s.id ? 'page' : undefined}
            title={p.sourceTitle(s)}
          >
            <span class="src-name">{s.name}</span>
            {#if kind}<span class="src-kind">{kind}</span>{/if}
          </a>
        {/each}
      </nav>

      <section class="listing" aria-label="Folder contents">
        {#if unknownSource}
          <div class="missing" role="alert" data-testid="unknown-source">
            <p>{p.unknownSource}</p>
            {#if p.unknownSourceDetails && sourceId !== null}
              <details>
                <summary>Details</summary>
                <code>{p.unknownSourceDetails(sourceId)}</code>
              </details>
            {/if}
          </div>
        {:else if source}
          <nav class="crumbs" aria-label="Folder path">
            {#each crumbs as c, i (c.path)}
              {#if i > 0}<span class="sep" aria-hidden="true">/</span>{/if}
              {#if c.kind === 'current'}<span aria-current="page">{c.label}</span>
                <!-- A folder the source hides (`biffix/` above a run's folder) cannot be opened. -->
              {:else if c.kind === 'hidden'}<span class="hidden-crumb">{c.label}</span>
              {:else}<a href={dataHref(source.id, c.path, undefined, project)}>{c.label}</a>{/if}
            {/each}
            {#if p.copyFolderPath}
              <button class="link copy" onclick={() => copied.copy(`${source.uri}${path}`)}
                >{copied.is(`${source.uri}${path}`) ? 'Copied' : 'Copy path'}</button
              >
            {/if}
          </nav>

          {#if run && p.analyses}
            <p class="provenance" data-testid="data-provenance">
              {p.analyses.resultOf}
              <a href={runTabHref(run.runId, 'overview')}>{runNames.name(run.runId) ?? run.runId}</a
              >
            </p>
          {/if}

          <DataToolbar presentation={p} {listing} {canUpload} {uploadOpen} onupload={openUpload} />

          {#if uploadOpen && canUpload && uploadPlace}
            {#key uploadSeq}<UploadPanel
                bind:this={panel}
                source={uploadPlace.source}
                folder={uploadFolder}
                project={uploadPlace.project}
                onstarted={started}
              />{/key}
          {/if}
          {#if batch && p.uploads}
            <p class="note" role="status" data-testid="upload-status">
              {#if batchMoving}
                {p.uploads.uploading(batch.uploads.length, batch.folder)}
              {:else if !batchDone}
                {p.uploads.held(batch.folder)}
              {:else if batchFilesDone > 0}
                {p.uploads.uploadedTo}
                <a href={dataHref(batch.source.id, batch.folder, undefined, batch.project)}
                  >{batch.folder}</a
                >.
              {:else}
                {p.uploads.stopped(batch.folder)}
              {/if}
            </p>
          {/if}

          {#if listing.error}
            <div class="error" role="alert">
              {listing.error}
              <button class="link" onclick={() => listing.retry()}>{p.retry}</button>
            </div>
          {/if}
          {#if downloadError}<p class="error" role="alert">{downloadError}</p>{/if}
          {#if openError}<p class="error" role="alert">{openError}</p>{/if}
          {#if missing}
            <p class="error" role="alert">
              {missing.error
                ? `Couldn't open ${file}: ${missing.error}`
                : `There is no file “${file}” in this folder.`}
              <button class="link" onclick={() => show(null)}>Dismiss</button>
            </p>
          {/if}

          {#if listing.searchResults}
            <p class="muted">
              {p.searchSummary(listing.searchResults.length, listing.searchTruncated)}
              <button class="link" onclick={() => (listing.searchResults = null)}
                >Back to the folder</button
              >
            </p>
          {/if}

          <div
            class="drop"
            class:dragging
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
            <FolderTable
              presentation={p}
              {source}
              {path}
              {project}
              {listing}
              {shortcuts}
              files={shownFiles}
              {opening}
              {copied}
              onshow={show}
              ondownload={download}
              onopen={openAsResult}
            />
          </div>
          {#if listing.loading}
            <p class="muted">Loading…</p>
          {:else if !listing.error && shownFiles.length === 0 && (listing.searchResults || (listing.folders.length === 0 && shortcuts.length === 0))}
            <p class="muted">{p.empty(searching, canUpload)}</p>
          {/if}
          {#if listing.cursor && !listing.searchResults && !listing.loading}
            <button class="btn btn-compact" onclick={() => listing.load(true)}>Show more</button>
          {/if}
          {#if p.analyses && path === RUNS_FOLDER}
            <p class="note">{p.analyses.runsFolderNote}</p>
          {/if}
        {/if}
      </section>
    </div>
  {/if}
</div>

{#if open}
  <ResultViewer target={open} onclose={() => (open = null)} />
{/if}
{#if viewing && source}
  <OutputViewer file={viewerFile(source, viewing)} onclose={() => show(null)} />
{/if}

<style>
  /* The wrapper only carries the view's name for the few styles that differ (the Lab view's
     sizes, its sources' kind under their name, a file's actions under it on a phone): it lays
     nothing out. */
  .data-browser {
    display: contents;
  }
  h1 {
    margin: 0;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.75rem;
    flex-wrap: wrap;
  }
  .project {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    font-size: 0.85rem;
    color: var(--text-muted);
  }
  .project select {
    font: inherit;
    padding: 0.3rem 0.5rem;
    border: 1px solid var(--control-border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text);
  }
  .lede {
    color: var(--text-muted);
    margin: 0.3rem 0 1rem;
    max-width: 48rem;
  }
  .browser {
    display: grid;
    grid-template-columns: minmax(10rem, 14rem) minmax(0, 1fr);
    gap: 1rem;
    align-items: start;
  }
  .sources {
    display: grid;
    gap: 0.15rem;
  }
  .sources a {
    display: flex;
    justify-content: space-between;
    gap: 0.4rem;
    padding: 0.4rem 0.6rem;
    border-radius: 0.4rem;
    color: var(--text);
    text-decoration: none;
    font-size: 0.85rem;
  }
  /* The Lab view puts what a source is under its name. */
  .lab .sources a {
    flex-direction: column;
    justify-content: normal;
    gap: 0.05rem;
    padding: 0.45rem 0.6rem;
    font-size: 0.88rem;
  }
  .sources a:hover {
    background: var(--hover);
  }
  .sources a.active {
    background: var(--active);
    font-weight: 600;
  }
  .src-kind {
    font-size: 0.68rem;
    color: var(--text-muted);
  }
  .lab .src-kind {
    font-size: 0.7rem;
    font-weight: normal;
  }
  .sources a.active .src-kind {
    color: inherit;
  }
  .crumbs {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    flex-wrap: wrap;
    font-size: 0.88rem;
    margin-bottom: 0.6rem;
  }
  .lab .crumbs {
    font-size: 0.9rem;
    margin-bottom: 0.5rem;
  }
  .crumbs a {
    color: var(--accent);
    text-decoration: none;
  }
  .crumbs .sep,
  .crumbs .hidden-crumb {
    color: var(--text-muted);
  }
  .crumbs .copy {
    margin-left: auto;
    font-size: 0.8rem;
  }
  .provenance {
    margin: 0 0 0.6rem;
    padding: 0.45rem 0.7rem;
    border: 1px solid var(--card-border);
    border-left: 4px solid var(--accent);
    border-radius: 0.45rem;
    font-size: 0.88rem;
  }
  .provenance a,
  .note a {
    color: var(--accent);
  }
  .note {
    margin: 0.4rem 0 0.6rem;
    font-size: 0.82rem;
    color: var(--text-muted);
  }
  .drop.dragging {
    outline: 2px dashed var(--accent);
    outline-offset: 2px;
  }
  /* Not underlined: the listing's actions read like its folder links. */
  .link {
    text-decoration: none;
    text-align: left;
  }
  button:focus-visible,
  a:focus-visible,
  select:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .error {
    font-size: 0.85rem;
  }
  .missing {
    font-size: 0.85rem;
  }
  /* The note sits close over its *Details*. */
  .missing p:has(+ details) {
    margin: 0 0 0.5rem;
  }
  .missing details {
    font-size: 0.78rem;
    color: var(--text-muted);
  }
  /* A phone stacks the places over the folder (the folder's table drops the date, FolderTable). */
  @media (max-width: 720px) {
    .browser {
      grid-template-columns: 1fr;
    }
    .sources {
      grid-auto-flow: column;
      grid-auto-columns: max-content;
      overflow-x: auto;
    }
  }
</style>
