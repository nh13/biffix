<script lang="ts">
  import { untrack } from 'svelte';
  import type { OutputPreview } from '#lib/api/index.js';
  import type { ViewerFile } from '#lib/components/viewerFile.js';
  import { errorMessage, fmtBytes } from '#lib/util/format.js';
  import { headerlessFormatOf, parseTable } from '#lib/util/tsv.js';
  import TsvTable from '#lib/components/TsvTable.svelte';
  import Dialog from '#lib/components/Dialog.svelte';

  // One file shown in the browser (a run output, or a file in a data source), by its `viewer`:
  // tables and text from the backend's preview (the first 1 MiB, decompressed), images / PDFs /
  // HTML from a short-lived S3 URL. HTML is untrusted (a workflow or a person wrote it), so it runs
  // in a sandboxed frame with an opaque origin: its scripts work, but it cannot reach this page, its
  // tokens, or the API.
  interface Props {
    file: ViewerFile;
    onclose?: () => void;
    /** Shown in the page (a run's Reports tab) instead of as a dialog: no backdrop or close. */
    embedded?: boolean;
    /** Where focus returns on close, when not whatever had it on open (see `Dialog`). */
    returnFocus?: HTMLElement | null;
  }
  let { file: output, onclose = () => {}, embedded = false, returnFocus }: Props = $props();

  /** Above this, an image waits for a click before loading. */
  const LARGE_IMAGE_BYTES = 50 * 1024 * 1024;

  const viewer = $derived(output.viewer ?? null);
  const inline = $derived(viewer === 'image' || viewer === 'pdf' || viewer === 'html');

  let preview = $state<OutputPreview | null>(null);
  let url = $state<string | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(true);
  let asText = $state(false);
  let loadLargeImage = $state(false);

  // Keyed on the id, not the object: a list refresh hands in a new object for the same output,
  // which must not reload (and reset) what is on screen.
  const outputId = $derived(output.key);
  $effect(() => {
    void outputId;
    const kind = inline;
    let cancelled = false;
    loading = true;
    error = null;
    preview = null;
    url = null;
    asText = false;
    // "Load it anyway" was for that image, not every large one this viewer shows next.
    loadLargeImage = false;
    const load = kind
      ? untrack(() => output.viewUrl()).then((u) => {
          if (!cancelled) url = u.url;
        })
      : untrack(() => output.preview()).then((p) => {
          if (!cancelled) preview = p;
        });
    load
      .catch((e) => {
        if (!cancelled) error = errorMessage(e);
      })
      .finally(() => {
        if (!cancelled) loading = false;
      });
    return () => {
      cancelled = true;
    };
  });

  // Which kind of table comes from the path, which always ends in the file's own name (a report's
  // `name` is its title).
  const table = $derived(
    preview && preview.kind === 'text' && viewer === 'table' && !asText
      ? parseTable(preview.text, preview.truncated, headerlessFormatOf(output.path))
      : null,
  );
  const largeImage = $derived(
    viewer === 'image' && output.size != null && output.size > LARGE_IMAGE_BYTES,
  );
</script>

{#if embedded}
  <section class="embedded" aria-labelledby="output-viewer-title">
    {@render panel()}
  </section>
{:else}
  <Dialog labelledby="output-viewer-title" {onclose} {returnFocus}>
    {@render panel()}
  </Dialog>
{/if}

{#snippet panel()}
  <header>
    <div class="title">
      <h3 id="output-viewer-title" class="mono">{output.name}</h3>
      <p class="muted meta">
        <span class="mono">{output.path}</span>
        {#if output.size != null}· {fmtBytes(output.size)}{/if}
        {#if output.detail}· {output.detail}{/if}
      </p>
    </div>
    <div class="actions">
      {#if table}
        <button type="button" class="link" onclick={() => (asText = true)}>raw text</button>
      {:else if asText}
        <button type="button" class="link" onclick={() => (asText = false)}>table</button>
      {/if}
      <button type="button" class="link" onclick={() => output.download()}>download</button>
      {#if !embedded}
        <button type="button" class="close" onclick={onclose} aria-label="Close">✕</button>
      {/if}
    </div>
  </header>

  <div class="body">
    {#if loading}
      <p class="muted">Loading…</p>
    {:else if error}
      <p class="error" role="alert">Couldn't open {output.name}: {error}</p>
    {:else if preview}
      {#if preview.kind === 'binary'}
        <p class="muted">
          This file isn't text, so it can't be shown here. Download it to open it locally.
        </p>
      {:else if table}
        <TsvTable {table} truncated={preview.truncated} />
      {:else}
        <pre class="text">{preview.text}</pre>
        {#if preview.truncated}
          <p class="muted notice">Showing the first 1 MiB of the file.</p>
        {/if}
      {/if}
    {:else if url}
      {#if viewer === 'image'}
        {#if largeImage && !loadLargeImage}
          <p class="muted">
            This image is {fmtBytes(output.size)}.
            <button type="button" class="link" onclick={() => (loadLargeImage = true)}
              >Load it anyway</button
            >
          </p>
        {:else}
          <img class="image" src={url} alt={output.name} />
        {/if}
      {:else if viewer === 'pdf'}
        <iframe class="frame" src={url} title={output.name} referrerpolicy="no-referrer"></iframe>
      {:else if viewer === 'html'}
        <!-- No allow-same-origin / top-navigation / forms / popups: an opaque, contained origin. -->
        <iframe
          class="frame"
          src={url}
          title={output.name}
          sandbox="allow-scripts allow-downloads"
          referrerpolicy="no-referrer"
        ></iframe>
      {/if}
    {/if}
  </div>
{/snippet}

<style>
  header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 1rem;
    padding: 0.75rem 1rem;
    border-bottom: 1px solid var(--border);
  }
  .title {
    min-width: 0;
  }
  h3 {
    margin: 0;
    font-size: 0.95rem;
    word-break: break-all;
  }
  .meta {
    margin: 0.2rem 0 0;
    font-size: 0.75rem;
    word-break: break-all;
  }
  /* Text links on the title's line: wider apart than buttons, and never wrapped under it. */
  .actions {
    flex-wrap: nowrap;
    gap: 0.75rem;
    white-space: nowrap;
  }
  .body {
    padding: 0.75rem 1rem 1rem;
    overflow: auto;
  }
  .text {
    margin: 0;
    padding: 0.5rem 0.75rem;
    max-height: 70vh;
    overflow: auto;
    font-family: var(--mono);
    font-size: 0.75rem;
    white-space: pre;
    background: var(--th-bg);
    border: 1px solid var(--border);
    border-radius: 0.35rem;
  }
  .notice {
    font-size: 0.75rem;
    margin: 0.5rem 0 0;
  }
  .image {
    display: block;
    max-width: 100%;
    max-height: 75vh;
    margin: 0 auto;
    /* The image's own background (often white) in a light frame, in either theme. */
    padding: 0.25rem;
    border-radius: 0.35rem;
    background: #fff;
    box-shadow: 0 0 0 1px var(--card-border);
    color-scheme: light;
  }
  .embedded {
    display: flex;
    flex-direction: column;
    background: var(--bg-content);
    border: 1px solid var(--border);
    border-radius: 0.5rem;
  }
  /* In the page, the report scrolls inside its own frame, never the panel around it. */
  .embedded .body {
    overflow: visible;
  }
  .embedded .frame {
    height: calc(100vh - 13rem);
    min-height: 32rem;
  }
  .frame {
    display: block;
    box-sizing: border-box;
    width: 100%;
    height: 75vh;
    border: 1px solid var(--border);
    border-radius: 0.35rem;
    /* A report is its own light page: a light canvas from the first paint (no dark flash, and no
       dark canvas behind a report that sets no background), whatever the app's theme. */
    background: #fff;
    color-scheme: light;
  }
  /* A dotted rule instead of an underline, as the viewer's other actions. */
  button.link {
    text-decoration: none;
    border-bottom: 1px dotted currentColor;
    font-size: 0.8rem;
  }
  .close {
    background: none;
    border: none;
    cursor: pointer;
    font-size: 1rem;
    color: var(--text-muted);
    padding: 0 0.25rem;
  }
  .close:hover {
    color: inherit;
  }
</style>
