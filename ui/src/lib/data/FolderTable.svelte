<script lang="ts">
  // The Data browser's folder: its shortcuts (the Lab view's), its folders (an analysis's named by
  // the analysis) and its files, or the search's results, each file with its actions — view it in
  // place, open it as an analysis's result (an alignment, a FastQC report), download it, copy its
  // path.
  import type { DataFile, DataSource } from '$lib/api';
  import type { CopiedFlag } from '$lib/util/clipboard.svelte';
  import { fmtBytes, fmtRelativeMicros } from '$lib/util/format';
  import { runNames } from '$lib/runs/runNames.svelte';
  import { runTabHref } from '$lib/runs/runNav';
  import { opensAs, resultOfOther } from './dataBrowser';
  import type { FolderListing } from './folderListing.svelte';
  import { runFolderId, type Shortcut } from './labPresentation';
  import { dataHref } from './paths';
  import type { DataPresentation } from './presentation';

  interface Props {
    presentation: DataPresentation;
    source: DataSource;
    /** The folder on show. */
    path: string;
    project: string | null;
    listing: FolderListing;
    /** The Lab view's shortcuts at the top of the project. */
    shortcuts: Shortcut[];
    /** The rows: the folder's files, or the search's results. */
    files: DataFile[];
    /** The file whose open-as-a-result is pending (its button waits). */
    opening: string | null;
    copied: CopiedFlag;
    onshow: (f: DataFile) => void;
    ondownload: (f: DataFile) => void;
    onopen: (f: DataFile, kind: 'alignment' | 'fastqc') => void;
  }
  let {
    presentation: p,
    source,
    path,
    project,
    listing,
    shortcuts,
    files,
    opening,
    copied,
    onshow,
    ondownload,
    onopen,
  }: Props = $props();

  const searching = $derived(!!listing.searchResults);
  const runFolder = (folderPath: string) => (p.analyses ? runFolderId(source, folderPath) : null);
  const resultOf = (f: DataFile) => (p.analyses ? resultOfOther(source, path, f.path) : null);
</script>

<table aria-busy={listing.loading}>
  <thead>
    <tr>
      <th scope="col">Name</th>
      <th scope="col" class="num">Size</th>
      <th scope="col" class="when">{p.changedColumn}</th>
      <th scope="col"><span class="sr-only">Actions</span></th>
    </tr>
  </thead>
  <tbody>
    {#if !listing.searchResults}
      {#each shortcuts as s (s.path)}
        <tr>
          <td colspan="4">
            <a class="folder" href={dataHref(source.id, s.path, undefined, project)}
              ><span aria-hidden="true">📁</span> {s.label}</a
            >
            <span class="sub">{s.note}</span>
          </td>
        </tr>
      {/each}
      {#each listing.folders as f (f.path)}
        {@const runId = runFolder(f.path)}
        <tr>
          <td colspan="4">
            <a class="folder" href={dataHref(source.id, f.path, undefined, project)}
              ><span aria-hidden="true">📁</span>
              {runId ? (runNames.name(runId) ?? f.name) : f.name}</a
            >
            {#if runId && runNames.name(runId)}<span class="sub mono">{f.name}</span>{/if}
          </td>
        </tr>
      {/each}
    {/if}
    {#each files as f (f.uri)}
      {@const kind = p.analyses ? opensAs(source, f) : null}
      {@const from = resultOf(f)}
      {@const name = searching ? f.path.slice(path.length) : f.name}
      <tr>
        <td class="name">
          {#if f.viewer}
            <!-- The in-place viewer stays a click away, whatever else the file opens as. -->
            <button class="link file" onclick={() => onshow(f)}>{name}</button>
          {:else}
            <span class="file">{name}</span>
          {/if}
          {#if from && p.analyses}<span class="sub"
              >{p.analyses.resultOf}
              <a href={runTabHref(from, 'overview')}>{runNames.name(from) ?? from}</a></span
            >{/if}
        </td>
        <td class="num">{fmtBytes(f.size)}</td>
        <td class="muted when">{fmtRelativeMicros(f.last_modified_micros)}</td>
        <td class="file-actions">
          {#if kind && p.analyses}
            <button
              class="btn btn-small btn-primary"
              disabled={opening === f.uri}
              onclick={() => onopen(f, kind)}
              >{kind === 'alignment' ? p.analyses.viewAlignments : p.analyses.openFastqc}<span
                class="sr-only"
              >
                {f.name}</span
              ></button
            >
          {/if}
          <button class="link" onclick={() => ondownload(f)}
            >Download<span class="sr-only"> {f.name}</span></button
          >
          <button class="link" onclick={() => copied.copy(f.uri)}
            >{copied.is(f.uri) ? 'Copied' : 'Copy path'}<span class="sr-only">
              of {f.name}</span
            ></button
          >
        </td>
      </tr>
    {/each}
  </tbody>
</table>

<style>
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.85rem;
  }
  th,
  td {
    text-align: left;
    padding: 0.4rem 0.55rem;
    border-bottom: 1px solid var(--border);
  }
  :global(.data-browser.lab) th,
  :global(.data-browser.lab) td {
    padding: 0.45rem 0.55rem;
  }
  th {
    font-size: 0.75rem;
    color: var(--text-muted);
    font-weight: 600;
  }
  .num {
    text-align: right;
    white-space: nowrap;
  }
  .name {
    overflow-wrap: anywhere;
  }
  .folder {
    color: var(--text);
    text-decoration: none;
    font-weight: 500;
  }
  .folder:hover {
    color: var(--accent);
  }
  .sub {
    display: block;
    margin-top: 0.1rem;
    font-size: 0.75rem;
    color: var(--text-muted);
  }
  .sub a {
    color: var(--accent);
  }
  .file {
    font-family: var(--mono);
    font-size: 0.8rem;
  }
  .file-actions {
    text-align: right;
    white-space: nowrap;
  }
  .file-actions > * + * {
    margin-left: 0.75rem;
  }
  /* Not underlined: file names and row actions read like the listing's folder links. */
  .link {
    text-decoration: none;
    text-align: left;
  }
  .link.file {
    font-family: var(--mono);
    font-size: 0.8rem;
  }
  button:focus-visible,
  a:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  /* A phone drops the date; the Lab view puts a file's actions under it. */
  @media (max-width: 720px) {
    .when {
      display: none;
    }
    :global(.data-browser.lab) tbody tr:has(td.file-actions) {
      display: grid;
      grid-template-columns: minmax(0, 1fr) auto;
    }
    :global(.data-browser.lab) tbody tr:has(td.file-actions) td.name {
      border-bottom: none;
    }
    :global(.data-browser.lab) td.file-actions {
      grid-column: 1 / -1;
      text-align: left;
      white-space: normal;
    }
  }
</style>
