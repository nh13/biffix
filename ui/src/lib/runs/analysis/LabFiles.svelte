<script lang="ts">
  // *All files* in the Lab view: every file an analysis wrote, by what it is (its key
  // results first, then its other results, then its logs), each with a plain name, its folder and
  // size, a note when it was reused from an earlier analysis, and Open (where a viewer can show
  // it), Show in Data, Download and Copy path. A search narrows it; a long group shows a page at a
  // time. No task or role columns and no work files: the Expert view's Outputs tab has those.
  import { goto } from '$app/navigation';
  import { runTabHref } from '$lib/runs/runNav';
  import type { RunDetail } from '$lib/api';
  import { CopiedFlag } from '$lib/util/clipboard.svelte';
  import { DataLinks } from '$lib/data/dataLinks.svelte';
  import { fmtBytes, plural } from '$lib/util/format';
  import { openTarget, type OpenTarget } from '$lib/qc/open';
  import ResultViewer from '$lib/qc/ResultViewer.svelte';
  import { getRunContext } from '$lib/runs/runContext.svelte';
  import { isLiveRun } from '$lib/runs/runState';
  import { OutputsLoader } from '$lib/runs/outputsLoader.svelte';
  import { site } from '$lib/state/site.svelte';
  import { setView } from '$lib/state/userPrefs.svelte';
  import { outputViewerFile } from '$lib/components/viewerFile';
  import {
    FILES_PER_PAGE,
    fileOpenKind,
    fileTitle,
    filterGroups,
    groupFiles,
    shownFiles,
    type FileGroupId,
    type LabFile,
  } from './files';
  import { runName } from '$lib/runs/runName';
  import { runNames } from '$lib/runs/runNames.svelte';

  let { run }: { run: RunDetail } = $props();

  const ctx = getRunContext();
  const runId = $derived(run.run_id);
  const live = $derived(isLiveRun(run));
  const name = $derived(runName(run));
  const analysisHref = $derived(runTabHref(run.run_id, 'overview'));

  $effect(() => {
    site.pageTitle = `All files · ${name}`;
    return () => {
      site.pageTitle = null;
    };
  });

  // Asked once, then again while the analysis runs (new files keep arriving). Each beat asks only
  // how many files there are (one row); the whole list is read again when that changes, and every
  // few beats anyway (a file's size or reuse can change with the count the same).
  const files = new OutputsLoader();
  const FULL_EVERY = 4;
  // An ended analysis is asked again, quickly, until every file is sized.
  $effect(() => files.watch({ runId, live, probeEvery: FULL_EVERY, waitForSizes: true }));
  const outputs = $derived(files.outputs);
  const total = $derived(files.total);
  const loading = $derived(files.loading);
  const error = $derived(files.error);

  let search = $state('');
  const groups = $derived(groupFiles(outputs, ctx.reports ?? []));
  const shownGroups = $derived(filterGroups(groups, search));
  const listed = $derived(groups.flatMap((g) => g.files));
  const fileCount = $derived(listed.length);
  const matchCount = $derived(shownGroups.reduce((n, g) => n + g.files.length, 0));
  const knownBytes = $derived(listed.reduce((n, f) => n + (f.output.size ?? 0), 0));
  // A file not sized yet (or whose size is unknown) is left out of the total, which then says so.
  const someUnsized = $derived(listed.some((f) => f.output.size == null));

  // How many pages of each group are shown; a new search starts each at its first page.
  let pages = $state<Record<string, number>>({});
  $effect(() => {
    void search;
    pages = {};
  });
  const pagesOf = (id: FileGroupId) => pages[id] ?? 1;

  // The analyses files were reused from, named.
  $effect(() => {
    runNames.want(outputs.flatMap((o) => (o.reused_from ? [o.reused_from] : [])));
  });

  const dataLinks = new DataLinks();
  $effect(() => {
    dataLinks.load(run.project);
  });
  const copied = new CopiedFlag();

  let open = $state<OpenTarget | null>(null);
  let downloadError = $state<string | null>(null);
  const viewerFile = (f: LabFile) =>
    outputViewerFile(runId, f.output, {
      name: fileTitle(f),
      detail: f.folder || null,
      onDownloadError: (m) => (downloadError = m),
    });
  function openFile(f: LabFile) {
    const kind = fileOpenKind(f);
    if (kind) open = openTarget(kind, runId, f.output, viewerFile(f));
  }

  /** For a bioinformatician: the Expert view's Outputs tab, which lists steps and work files
   * too. */
  async function toExpert() {
    void setView('expert');
    await goto(`${analysisHref}/outputs`);
  }
</script>

<div class="topline">
  <a class="back" href={analysisHref}>← {name}</a>
</div>
<header class="head">
  <h1>All files</h1>
  <p class="sub">
    {#if loading}Loading…{:else}{plural(fileCount, 'file')}{#if knownBytes > 0}{someUnsized
          ? ' · at least '
          : ' · '}{fmtBytes(knownBytes)}{/if}{#if total > outputs.length}{' · '}the first {outputs.length.toLocaleString()}
        of {total.toLocaleString()}{/if}{/if}
  </p>
</header>

<div class="tools">
  <input
    type="search"
    placeholder="Find a file or sample"
    aria-label="Find a file"
    bind:value={search}
  />
  {#if search.trim() && !loading}
    <span class="muted" role="status"
      >{matchCount.toLocaleString()} of {plural(fileCount, 'file')}</span
    >
  {/if}
</div>

{#if error}<p class="error" role="alert">Couldn't list the files: {error}</p>{/if}
{#if downloadError}<p class="error" role="alert">{downloadError}</p>{/if}

{#if !loading && !error && fileCount === 0}
  <p class="muted">
    {live
      ? 'No files yet: they appear here as the analysis writes them.'
      : 'This analysis wrote no files.'}
  </p>
{:else if !loading && search.trim() && matchCount === 0}
  <p class="muted">
    No file matches “{search.trim()}”{#if total > outputs.length}{' '}in the first {plural(
        outputs.length,
        'file',
      )}{/if}.
  </p>
{/if}

{#each shownGroups as group (group.id)}
  {@const page = shownFiles(group.files, pagesOf(group.id))}
  <section class="group" aria-labelledby="files-{group.id}">
    <h2 id="files-{group.id}">
      {group.title} <span class="count">{group.files.length}</span>
    </h2>
    <p class="note">{group.note}</p>
    <ul class="files">
      {#each page.shown as f (f.output.id)}
        {@const o = f.output}
        {@const kind = fileOpenKind(f)}
        {@const title = fileTitle(f)}
        {@const dataHref = o.exists !== false ? dataLinks.href(o.s3_uri) : null}
        <li class="file" data-testid="lab-file">
          <div class="what">
            <span class="name">{title}</span>
            <span class="meta">
              {#if f.result}<span class="file-name">{o.name}</span>{/if}
              {#if f.folder}<span class="folder">in {f.folder}</span>{/if}
              <span class="size">{fmtBytes(o.size)}</span>
              {#if o.reused_from}<span class="reused"
                  >Reused from <a href={runTabHref(o.reused_from, 'overview')}
                    >{runNames.name(o.reused_from) ?? o.reused_from}</a
                  ></span
                >{:else if o.reused}<span class="reused">Reused from an earlier analysis</span>{/if}
              {#if o.exists === false}<span class="gone">No longer stored</span>{/if}
            </span>
          </div>
          <div class="actions">
            {#if kind}<button class="btn btn-compact btn-primary" onclick={() => openFile(f)}
                >{kind === 'alignment' ? 'View alignments' : 'Open'}<span class="sr-only">
                  {title}</span
                ></button
              >{/if}
            {#if dataHref}<a class="btn btn-compact" href={dataHref}
                >Show in Data<span class="sr-only"> browser: {title}</span></a
              >{/if}
            {#if o.servable && o.exists !== false}<button
                class="btn btn-compact"
                onclick={() => viewerFile(f).download()}
                >Download<span class="sr-only"> {title}</span></button
              >{/if}
            <button class="btn btn-compact quiet" onclick={() => copied.copy(o.s3_uri, o.id)}
              >{copied.is(o.id) ? 'Copied' : 'Copy path'}<span class="sr-only">
                of {title}</span
              ></button
            >
          </div>
        </li>
      {/each}
    </ul>
    {#if page.left > 0}
      <p class="more">
        <span class="muted">Showing {page.shown.length} of {group.files.length}.</span>
        <button
          class="btn btn-compact"
          onclick={() => (pages = { ...pages, [group.id]: pagesOf(group.id) + 1 })}
          >Show {Math.min(page.left, FILES_PER_PAGE)} more<span class="sr-only">
            of {group.title}</span
          ></button
        >
      </p>
    {/if}
  </section>
{/each}

<p class="bioinf">
  For a bioinformatician: <button class="link" onclick={toExpert}
    >the Expert view's Outputs tab</button
  > lists each file with the step that wrote it, and the work files.
</p>

{#if open}
  <ResultViewer target={open} onclose={() => (open = null)} />
{/if}

<style>
  .topline {
    margin-bottom: 0.3rem;
  }
  .back {
    font-size: 0.8rem;
    color: var(--text-muted);
    text-decoration: none;
  }
  .head h1 {
    margin: 0;
  }
  .sub {
    margin: 0.2rem 0 0.8rem;
    font-size: 0.85rem;
    color: var(--text-muted);
  }
  .tools {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.5rem 0.8rem;
    margin-bottom: 1rem;
  }
  .tools input {
    font: inherit;
    font-size: 0.88rem;
    padding: 0.4rem 0.6rem;
    border: 1px solid var(--control-border);
    border-radius: 0.45rem;
    background: var(--bg-content);
    color: var(--text);
    width: min(100%, 22rem);
  }
  .group {
    margin-bottom: 1.5rem;
  }
  .group h2 {
    margin: 0;
    font-size: 1.05rem;
  }
  .count {
    font-size: 0.8rem;
    font-weight: normal;
    color: var(--text-muted);
  }
  .note {
    margin: 0.15rem 0 0.5rem;
    font-size: 0.8rem;
    color: var(--text-muted);
  }
  .files {
    margin: 0;
    padding: 0;
    list-style: none;
    border-top: 1px solid var(--border);
  }
  .file {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 0.35rem 1rem;
    padding: 0.5rem 0.2rem;
    border-bottom: 1px solid var(--border);
  }
  .what {
    display: grid;
    gap: 0.1rem;
    min-width: 0;
    flex: 1 1 18rem;
  }
  .name {
    font-weight: 500;
    overflow-wrap: anywhere;
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: 0.1rem 0.7rem;
    font-size: 0.78rem;
    color: var(--text-muted);
  }
  .file-name,
  .folder {
    font-family: var(--mono);
    overflow-wrap: anywhere;
  }
  .meta a {
    color: var(--accent);
  }
  .gone {
    color: var(--fg-fail);
  }
  /* Copy path: a button that reads as a link but keeps the button's size, so it lines up. */
  .quiet {
    border-color: transparent;
    color: var(--accent);
    padding-inline: 0.3rem;
  }
  .more {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.6rem;
    margin: 0.6rem 0 0;
    font-size: 0.85rem;
  }
  .muted {
    font-size: 0.85rem;
  }
  .error {
    font-size: 0.85rem;
  }
  .bioinf {
    margin: 1.2rem 0 0;
    font-size: 0.8rem;
    color: var(--text-muted);
  }
  button:focus-visible,
  a:focus-visible,
  input:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  /* A phone puts each file's actions under its name, the buttons wide enough to tap. */
  @media (max-width: 640px) {
    .file {
      align-items: stretch;
    }
    .actions {
      width: 100%;
    }
    .btn {
      padding: 0.45rem 0.8rem;
    }
  }
</style>
