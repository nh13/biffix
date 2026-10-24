<script lang="ts">
  import { beforeNavigate } from '$app/navigation';
  import { getOutput, type RunOutput } from '#lib/api/index.js';
  import { outputViewerFile, type ViewerFile } from '#lib/components/viewerFile.js';
  import { debounce } from '#lib/util/debounce.js';
  import { UrlEcho } from '#lib/util/urlEcho.js';
  import { focusRadio, radioKeyTarget } from '#lib/util/radioGroup.js';
  import { errorMessage, fmtBytes, plural } from '#lib/util/format.js';
  import { errorKind } from '#lib/components/errorPage.js';
  import { CopiedFlag } from '#lib/util/clipboard.svelte.js';
  import { s3ConsoleUrl } from '#lib/util/links.js';
  import { DataLinks } from '#lib/data/dataLinks.svelte.js';
  import OutputViewer from '#lib/components/OutputViewer.svelte';
  import ReusedFrom from '#lib/runs/ReusedFrom.svelte';
  import type { OutputsView } from '#lib/runs/outputsUrl.js';
  import { OutputsLoader } from '#lib/runs/outputsLoader.svelte.js';
  import {
    buildOutputTree,
    initiallyOpen,
    matchesSearch,
    reusedFromSummary,
    roleLabel,
    visibleRows,
  } from '#lib/runs/outputs.js';
  // The run's registered outputs, as a directory tree (relative to its output prefix) or a flat
  // list, with per-file download / copy / console actions. Fetched on its own endpoint, so the run
  // page's poll stays cheap; re-fetched while the run is live.
  interface Props {
    runId: string;
    /** The run's project, whose data sources the Data page shows its outputs in. */
    project?: string | null;
    engine?: string | null;
    /** The run's non-intermediate output count, from its summary (for the heading). */
    outputsTotal: number;
    /** Whether the run is still going: outputs keep arriving, so re-fetch periodically. */
    live: boolean;
    /** Only this task's outputs (set from a task row); null ⇒ all. */
    task: string | null;
    onclearTask: () => void;
    onjumpTask: (taskKey: string) => void;
    /**
     * The view, search, work-files toggle and open file, when a caller keeps them (e.g. in the
     * URL); `onstate` then reports each change. Without it the component keeps its own.
     */
    urlState?: OutputsView;
    onstate?: (next: OutputsView) => void;
  }
  let {
    runId,
    project = null,
    engine,
    outputsTotal,
    live,
    task,
    onclearTask,
    onjumpTask,
    urlState,
    onstate,
  }: Props = $props();

  const files = new OutputsLoader();
  const outputs = $derived(files.outputs);
  const total = $derived(files.total);
  const pending = $derived(files.pending);
  const sizing = $derived(files.sizing);
  const loading = $derived(files.loading);
  const error = $derived(files.error);
  let showWork = $state(false);
  let view = $state<'tree' | 'flat'>('tree');
  let search = $state('');
  let open = $state<Set<string> | null>(null);
  // The open file, by id (so a refresh or a link can reopen it); the row is looked up in `outputs`.
  let viewingId = $state<string | null>(null);
  // A linked file past the listed rows (a run with more than MAX_ROWS outputs) is fetched by id.
  let fetchedFile = $state<RunOutput | null>(null);
  const listedFile = $derived(viewingId ? (outputs.find((o) => o.id === viewingId) ?? null) : null);
  const viewing = $derived(
    listedFile ?? (fetchedFile && fetchedFile.id === viewingId ? fetchedFile : null),
  );
  // Why a linked file could not be fetched (a stale link, not an output of this run, opens
  // nothing and says nothing).
  let linkError = $state<string | null>(null);
  $effect(() => {
    const id = viewingId;
    linkError = null;
    if (!id || loading || listedFile) return;
    let stale = false;
    getOutput(runId, id)
      .then((o) => {
        if (!stale) fetchedFile = o;
      })
      .catch((e) => {
        if (!stale && errorKind(e) !== 'not-found')
          linkError = `Couldn't open the linked file: ${errorMessage(e)}`;
      });
    return () => {
      stale = true;
    };
  });

  // A caller-kept state drives the controls; each control change reports back through `onstate`.
  const urlView = $derived(urlState?.view);
  const urlWork = $derived(urlState?.work);
  const urlQ = $derived(urlState?.q);
  const urlFile = $derived(urlState?.file);
  $effect(() => {
    if (urlView !== undefined) view = urlView;
  });
  $effect(() => {
    if (urlWork !== undefined) showWork = urlWork;
  });
  // The URL's `q` comes back trimmed, a round trip after this component reported it: that echo is
  // not copied into the box (`UrlEcho`); any other change of the URL (back/forward, a link) is.
  const echo = new UrlEcho();
  $effect(() => {
    if (urlQ !== undefined && !echo.isEcho(urlQ)) search = urlQ;
  });
  $effect(() => {
    if (urlFile !== undefined) viewingId = urlFile;
  });
  function report(patch: Partial<OutputsView>) {
    if (!onstate) return;
    const next = { view, q: search, work: showWork, task, file: viewingId, ...patch };
    echo.reported(next.q.trim(), urlQ ?? '');
    onstate(next);
  }
  const onsearch = debounce(() => report({ q: search }), 250);
  // Leaving the view drops a report still waiting: fired during the navigation away it would replace
  // that navigation, and fired after it would rewrite the next page's URL. Every report carries the
  // whole state, search included, so a navigation of this view's own loses nothing.
  beforeNavigate(() => onsearch.cancel());
  $effect(() => onsearch.cancel);
  function setView(v: 'tree' | 'flat') {
    view = v;
    report({ view: v });
  }
  const VIEWS = ['tree', 'flat'] as const;
  /** A radio group is one Tab stop; the arrow keys (and Home / End) move the choice. */
  function viewKeydown(e: KeyboardEvent) {
    const to = radioKeyTarget(e.key, VIEWS.indexOf(view), VIEWS.length);
    if (to === null) return;
    e.preventDefault();
    setView(VIEWS[to]);
    focusRadio(e, to);
  }
  function openFile(id: string | null) {
    viewingId = id;
    report({ file: id });
  }

  // A different run or task is a different listing: drop the last one (so it is not shown, or used
  // to pick the tree's initially open directories, while the new one loads). Toggling work files
  // re-fetches the same listing and keeps what is shown and opened.
  let listing: string | null = null;
  $effect(() => {
    const key = JSON.stringify([runId, task]);
    if (key === listing) return;
    const first = listing === null;
    listing = key;
    if (first) return;
    files.clear();
    open = null;
  });

  $effect(() => {
    // Re-run (a fresh fetch, then polling only while live, or until an ended run's sizes are all
    // in) when the run, the task, the role set or liveness changes. `*`: every role, intermediates
    // and roles this UI does not know included (an open enum).
    const query = { task: task ?? undefined, roles: showWork ? ['*'] : [] };
    return files.watch({ runId, live, query, waitForSizes: true });
  });

  const filtered = $derived(outputs.filter((o) => matchesSearch(o, search)));
  const tree = $derived(buildOutputTree(filtered));
  // Open the top two levels the first time there is something to show; afterwards the viewer's
  // expand/collapse choices stand (a refresh must not re-collapse what they opened).
  $effect(() => {
    if (open === null && outputs.length > 0) open = initiallyOpen(buildOutputTree(outputs), 2);
  });
  // A search or task filter shows every match, not just those in open directories.
  const narrowed = $derived(search.trim().length > 0 || task !== null);
  // A flat list of files (each named by its full path), not the tree.
  const flat = $derived(view === 'flat' || narrowed);
  const rows = $derived(
    flat
      ? filtered.map((file) => ({ kind: 'file' as const, file, depth: 0 }))
      : visibleRows(tree, open ?? new Set()),
  );
  const reusedCount = $derived(filtered.filter((o) => o.reused).length);
  const missingCount = $derived(filtered.filter((o) => o.exists === false).length);
  const summary = $derived(
    [
      plural(filtered.length, 'file') +
        (total > outputs.length ? ` (first ${outputs.length} of ${total})` : ''),
      tree.knownBytes > 0 ? `${fmtBytes(tree.knownBytes)} known` : '',
      reusedCount > 0 ? `${reusedCount} reused from cache` : '',
      reusedFromSummary(filtered),
      missingCount > 0 ? `${missingCount} missing` : '',
      // Sizes the loader stopped waiting for are unknown, not still coming.
      pending > 0 ? (sizing ? 'sizing…' : `${plural(pending, 'size')} unknown`) : '',
    ]
      .filter(Boolean)
      .join(' · '),
  );

  function toggleDir(path: string) {
    const next = new Set(open ?? []);
    if (next.has(path)) next.delete(path);
    else next.add(path);
    open = next;
  }

  /** Whether the browser can show `o`: a known viewer, served by biffix, still in S3. */
  function canView(o: RunOutput): boolean {
    return !!o.viewer && o.servable && o.exists !== false;
  }

  let downloadError = $state<string | null>(null);
  /** An output as the viewer shows it (and as a row's download button fetches it). */
  const viewerFile = (o: RunOutput): ViewerFile =>
    outputViewerFile(runId, o, { detail: o.task_key, onDownloadError: (m) => (downloadError = m) });

  const copied = new CopiedFlag();
  const copyUri = (o: RunOutput) => copied.copy(o.s3_uri, o.id);

  // Each output's place in the Data page, when the Data page can show it.
  const dataLinks = new DataLinks();
  $effect(() => {
    dataLinks.load(project);
  });
</script>

<section class="outputs">
  <h2>Outputs ({outputsTotal})</h2>
  <div class="controls">
    <input
      class="search"
      type="search"
      placeholder="Filter by path or task"
      aria-label="Filter outputs"
      bind:value={search}
      oninput={onsearch}
    />
    <label title="Show the work-directory files tasks passed to each other (usually many)">
      <input type="checkbox" bind:checked={showWork} onchange={() => report({ work: showWork })} /> work
      files
    </label>
    <div class="view" role="radiogroup" aria-label="Outputs view">
      {#each VIEWS as v (v)}
        <button
          type="button"
          role="radio"
          aria-checked={view === v}
          tabindex={view === v ? 0 : -1}
          onclick={() => setView(v)}
          onkeydown={viewKeydown}>{v}</button
        >
      {/each}
    </div>
  </div>

  {#if task}
    <p class="muted filter-note">
      Showing outputs of <strong>{task}</strong> —
      <button class="linkish" onclick={onclearTask}>show all</button>
    </p>
  {/if}

  {#if loading && outputs.length === 0}
    <p class="muted">Loading outputs…</p>
  {:else if error && outputs.length === 0}
    <p class="error">Couldn't load outputs: {error}</p>
  {:else if outputs.length === 0}
    <p class="muted">
      {#if live}
        No outputs registered yet — they appear as tasks finish.
      {:else if showWork}
        No outputs registered for this run.
      {:else}
        No outputs registered. Its engine plugin may not report outputs, or everything it wrote is a
        work file (tick “work files”).
      {/if}
    </p>
  {:else}
    <p class="summary muted">{summary}</p>
    {#if downloadError}<p class="error" role="alert">{downloadError}</p>{/if}
    {#if linkError}<p class="error" role="alert">{linkError}</p>{/if}
    {#if rows.length === 0}
      <p class="muted">No outputs match this filter.</p>
    {:else}
      <div class="table-scroll">
        <table>
          <thead>
            <tr><th>Name</th><th>Size</th><th>Task</th><th></th></tr>
          </thead>
          <tbody>
            {#each rows as row (row.kind === 'dir' ? `d:${row.dir.path}` : `f:${row.file.id}`)}
              {#if row.kind === 'dir'}
                <tr class="dir">
                  <td style:padding-left={`${0.5 + row.depth * 1.1}rem`}>
                    <button
                      class="linkish dirbtn"
                      aria-expanded={row.open}
                      onclick={() => toggleDir(row.dir.path)}
                      >{row.open ? '▾' : '▸'} {row.dir.name}/</button
                    >
                    <span class="muted count">{row.dir.fileCount}</span>
                  </td>
                  <td class="size">{row.dir.knownBytes > 0 ? fmtBytes(row.dir.knownBytes) : ''}</td>
                  <td></td>
                  <td></td>
                </tr>
              {:else}
                {@const o = row.file}
                <!-- Its place in the Data page, once per row; none for an output gone from S3. -->
                {@const inData = o.exists !== false ? dataLinks.href(o.s3_uri) : null}
                {@const consoleUrl = s3ConsoleUrl(o.s3_uri)}
                <tr>
                  <td
                    class="name"
                    style:padding-left={`${0.5 + row.depth * 1.1}rem`}
                    title={o.s3_uri}
                  >
                    {#if canView(o)}<button
                        class="linkish mono"
                        title="View in the browser"
                        onclick={() => openFile(o.id)}>{flat ? o.path : o.name}</button
                      >{:else}<span class="mono">{flat ? o.path : o.name}</span>{/if}
                    {#if o.role !== 'output'}<span class="tag role">{roleLabel(o.role)}</span>{/if}
                    {#if o.reused}<span
                        class="tag reused"
                        title="Satisfied from the engine cache, not produced by this run"
                        >reused</span
                      >{/if}
                    {#if o.reused_from}<span class="from"><ReusedFrom from={o.reused_from} /></span
                      >{/if}
                    {#if o.exists === false}<span
                        class="tag missing"
                        title="No longer in S3: removed after the run (e.g. a temp() file) or deleted"
                        >missing</span
                      >{/if}
                    {#if o.modified_since_produced}<span
                        class="tag modified"
                        title="Changed in S3 since this run wrote it, e.g. overwritten by a later run sharing the output prefix"
                        >modified</span
                      >{/if}
                  </td>
                  <td class="size">{fmtBytes(o.size)}</td>
                  <td class="task">
                    {#if o.task_key}<button class="linkish" onclick={() => onjumpTask(o.task_key!)}
                        >{o.task_key}</button
                      >{:else}<span class="muted" title="Not attributed to a single task">run</span
                      >{/if}
                  </td>
                  <td class="file-actions">
                    {#if o.exists === false}<span class="muted" title="Not in S3 any more"
                        >gone</span
                      >{:else if o.servable}<button
                        class="link"
                        onclick={() => viewerFile(o).download()}>download</button
                      >{:else}<span
                        class="muted"
                        title="Outside the project bucket (or a reserved prefix): biffix does not serve it"
                        >not served</span
                      >{/if}
                    {#if inData}<a
                        class="link data"
                        href={inData}
                        title="Show it in the Data browser"
                        >in Data<span class="sr-only"> browser: {o.path}</span></a
                      >{/if}
                    <button class="link" onclick={() => copyUri(o)}
                      >{copied.is(o.id) ? 'copied' : 'copy URI'}</button
                    >
                    {#if consoleUrl}<a
                        class="link aws"
                        href={consoleUrl}
                        target="_blank"
                        rel="noreferrer"
                        title="Open in the S3 console (requires AWS access)">AWS ↗</a
                      >{/if}
                  </td>
                </tr>
              {/if}
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  {/if}
  {#if viewing}
    <OutputViewer file={viewerFile(viewing)} onclose={() => openFile(null)} />
  {/if}
  {#if engine === 'snakemake'}
    <p class="hint">
      Snakemake lists only what this run wrote: outputs it reused from an earlier run (already up to
      date) are not tracked.
    </p>
  {/if}
</section>

<style>
  .outputs {
    margin: 1.5rem 0;
  }
  .controls {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.75rem;
    margin-bottom: 0.5rem;
    font-size: 0.8rem;
  }
  .search {
    flex: 1 1 14rem;
    max-width: 24rem;
    padding: 0.3rem 0.5rem;
    border: 1px solid var(--border);
    border-radius: 0.35rem;
    background: var(--bg-content);
    color: inherit;
  }
  .view {
    display: flex;
    gap: 0.25rem;
  }
  .view button {
    font-size: 0.72rem;
    padding: 0.15rem 0.5rem;
    border: 1px solid var(--border);
    border-radius: 0.35rem;
    background: none;
    color: var(--text-muted);
    cursor: pointer;
  }
  .view button[aria-checked='true'] {
    color: inherit;
    border-color: var(--text-muted);
  }
  .summary,
  .filter-note {
    font-size: 0.8rem;
    margin: 0.25rem 0 0.5rem;
  }
  .table-scroll {
    overflow-x: auto;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.8rem;
  }
  th {
    text-align: left;
    font-weight: 600;
    color: var(--text-muted);
    border-bottom: 1px solid var(--border);
    padding: 0.35rem 0.5rem;
  }
  td {
    padding: 0.3rem 0.5rem;
    border-bottom: 1px solid var(--border);
    vertical-align: top;
  }
  .name {
    word-break: break-all;
  }
  .size {
    white-space: nowrap;
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .task {
    max-width: 16rem;
    word-break: break-all;
  }
  .file-actions {
    white-space: nowrap;
  }
  .file-actions > * + * {
    margin-left: 0.6rem;
  }
  .dirbtn {
    font-weight: 600;
  }
  .count {
    margin-left: 0.35rem;
    font-size: 0.72rem;
  }
  .tag {
    margin-left: 0.4rem;
    font-size: 0.68rem;
    padding: 0.05rem 0.35rem;
    border: 1px solid var(--border);
    border-radius: 0.3rem;
    color: var(--text-muted);
  }
  .from {
    margin-left: 0.4rem;
  }
  .tag.missing,
  .tag.modified {
    color: var(--fg-warn);
    border-color: currentColor;
  }
  .link {
    font-size: 0.72rem;
    text-decoration: none;
    border-bottom: 1px dotted currentColor;
  }
  .link:hover {
    color: var(--accent);
  }
  /* A dotted rule instead of an underline, as the table's link actions. */
  button.link {
    text-decoration: none;
    font-size: 0.72rem;
    border-bottom: 1px dotted currentColor;
  }
  .link.aws {
    color: var(--text-muted);
  }
  .linkish {
    background: none;
    border: none;
    color: var(--accent);
    cursor: pointer;
    padding: 0;
    font: inherit;
    text-align: left;
  }
  /* .linkish resets the font, so a file name restates the shared .mono after it. */
  .linkish.mono {
    font-family: var(--mono);
  }
  .linkish:hover {
    text-decoration: underline;
  }
  .dirbtn {
    color: inherit;
  }
  .hint {
    margin-top: 0.5rem;
  }
</style>
