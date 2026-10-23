<script lang="ts">
  import type { RunDetail } from '$lib/api';
  import { s3ConsoleUrl } from '$lib/util/links';
  import { resumeCommand } from '$lib/runs/resume';
  import { runTabHref } from '$lib/runs/runNav';
  import { fmtRelativeMicros, plural } from '$lib/util/format';
  import { CopiedFlag } from '$lib/util/clipboard.svelte';
  import { imageMapRows, submitImageRows } from './runFacts';
  import S3Location from '$lib/data/S3Location.svelte';

  // Where the run writes and what it read: its output prefix, the Nextflow session and resume
  // command, and its input URIs.
  interface Props {
    run: RunDetail;
  }
  let { run }: Props = $props();

  // A run's outputs are isolated under `…/biffix/runs/<run_id>/` unless it was submitted with
  // `--output-prefix` (flagged: that prefix may be shared with other runs) or it resumed another run
  // (it writes into that run's prefix, by design).
  // The server classifies the prefix (`prefix_kind`).
  const outputPrefixKind = $derived(run.prefix_kind ?? null);

  // Resume: where a Nextflow run's cache lives (from the server), and the command that resumes a
  // finished run.
  const cacheUri = $derived(run.resume_cache_uri ?? null);
  const cacheUrl = $derived(s3ConsoleUrl(cacheUri));
  const resumeCmd = $derived(resumeCommand(run));

  // Intermediate files (a Nextflow run's work dir): gone once the retention sweep deleted them, or
  // what the run asked for when it isn't the deploy's default.
  const intermediates = $derived(
    run.work_deleted_at_micros
      ? `deleted ${fmtRelativeMicros(run.work_deleted_at_micros)} — results are kept, but the run can no longer be resumed`
      : run.retention === 'delete_on_success'
        ? 'deleted when the run succeeds'
        : run.retention === 'keep'
          ? 'kept'
          : null,
  );
  // What the run ran with, from its submit record: source commit, engine runtime, images. A run
  // that never launched has no submit record, only the images it was submitted with (it will run
  // with them, or failed preparing them).
  const prov = $derived(run.provenance ?? null);
  const mapRows = $derived(imageMapRows(prov?.image_map));
  const submitRows = $derived(submitImageRows(run.submit_images));

  const copied = new CopiedFlag();
  function copyResume() {
    if (resumeCmd) void copied.copy(resumeCmd, 'resume');
  }
</script>

{#if run.output_prefix}
  <div class="meta single">
    <div class="meta-item">
      <span class="meta-label">Output prefix</span>
      <span class="meta-value mono">
        <S3Location
          uri={run.output_prefix}
          project={run.project}
          what="the output prefix"
          aws
          testid="output-prefix"
        />
        {#if outputPrefixKind === 'custom'}<span
            class="tag"
            title="Submitted with --output-prefix: outputs land at a caller-chosen prefix, which other runs may share (so they are not isolated to this run)"
            >custom prefix</span
          >{:else if outputPrefixKind === 'resumed'}<span
            class="tag"
            title={`Resumed ${run.resumed_from}, so this run writes into that run's prefix (work dir and cache)`}
            >shared (resumed)</span
          >{/if}
      </span>
    </div>
  </div>
{/if}

{#if run.resumed_from || (run.engine === 'nextflow' && run.engine_session_id) || intermediates || resumeCmd}
  <div class="meta single">
    {#if run.resumed_from}
      <div class="meta-item">
        <span class="meta-label">Resumed from</span>
        <span class="meta-value mono">
          <a href={runTabHref(run.resumed_from, 'overview')}>{run.resumed_from}</a>
        </span>
      </div>
    {/if}
    {#if run.engine === 'nextflow' && run.engine_session_id}
      <div class="meta-item">
        <span class="meta-label">Session</span>
        <span class="meta-value mono">
          {#if cacheUrl}
            <a href={cacheUrl} target="_blank" rel="noreferrer" title={`Resume cache: ${cacheUri}`}
              >{run.engine_session_id}</a
            >
          {:else}{run.engine_session_id}{/if}
          {#if run.session_mismatch}<span
              class="tag warn"
              title="Nextflow reported a different session than biffix launched it with; this is the engine's own id"
              >session mismatch</span
            >{/if}
        </span>
      </div>
    {/if}
    {#if intermediates}
      <div class="meta-item">
        <span class="meta-label">Intermediate files</span>
        <span class="meta-value">{intermediates}</span>
      </div>
    {/if}
    {#if resumeCmd}
      <div class="meta-item">
        <span class="meta-label">Resume</span>
        <span class="meta-value mono resume">
          <code>{resumeCmd}</code>
          <button class="copy" onclick={copyResume}
            >{copied.is('resume') ? 'copied' : 'copy'}</button
          >
        </span>
      </div>
    {/if}
  </div>
{/if}

{#if prov || submitRows.length > 0}
  <section class="ran-with">
    <h2>
      {prov ? 'Ran with' : run.preparing ? 'Will run with' : 'Submitted with'}
    </h2>
    {#if prov}<div class="meta">
        {#if prov.git_sha}
          <div class="meta-item">
            <span class="meta-label">Source commit</span>
            <span class="meta-value mono" title={prov.git_sha}>{prov.git_sha.slice(0, 12)}</span>
          </div>
        {/if}
        {#if prov.runtime}
          <div class="meta-item">
            <span class="meta-label">Engine runtime</span>
            <span
              class="meta-value mono"
              title={`Runtime ${prov.runtime.runtime_key}\nCoordinator image ${prov.runtime.coordinator_image}`}
              >{run.engine ?? 'engine'}
              {prov.runtime.engine_version} · {prov.runtime.runtime_key.slice(0, 12)}</span
            >
          </div>
        {/if}
        {#if prov.image_digest}
          <div class="meta-item">
            <span class="meta-label">Default image</span>
            <span class="meta-value mono">
              {prov.image_digest}
              {#if prov.image_source_digest}<span
                  class="derived"
                  title="The imported image with the engine runtime added"
                >
                  derived from {prov.image_source_digest}</span
                >{/if}
            </span>
          </div>
        {/if}
      </div>{/if}
    {#if submitRows.length > 0}
      <div class="image-map submit-images" data-testid="submit-images">
        <h3>Submit-time images</h3>
        <table>
          <thead><tr><th>Given as</th><th>Pinned to</th></tr></thead>
          <tbody>
            {#each submitRows as row (row.declared)}
              <tr>
                <td class="mono">{row.declared}</td>
                <td class="mono">{row.pinned}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
    {#if mapRows.length > 0}
      <details class="image-map">
        <summary>Image map ({plural(mapRows.length, 'image')})</summary>
        <table>
          <thead><tr><th>Declared as</th><th>Ran</th></tr></thead>
          <tbody>
            {#each mapRows as row (row.image)}
              <tr>
                <td class="mono"
                  >{#each row.refs as ref}<div>{ref}</div>{/each}</td
                >
                <td class="mono">{row.image}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </details>
    {/if}
  </section>
{/if}

{#if run.input_uris && run.input_uris.length > 0}
  <section class="inputs">
    <h2>Reads from</h2>
    <ul class="uri-list">
      {#each run.input_uris as uri}
        <li>{uri}</li>
      {/each}
    </ul>
  </section>
{/if}

<style>
  .ran-with {
    margin-bottom: 1.25rem;
  }
  .ran-with h2 {
    margin-bottom: 0.4rem;
  }
  .ran-with .meta {
    margin-bottom: 0.5rem;
  }
  .derived {
    display: block;
    color: var(--text-muted);
  }
  .image-map summary {
    cursor: pointer;
    font-size: 0.82rem;
    color: var(--text-muted);
  }
  .submit-images {
    margin-bottom: 0.5rem;
  }
  .submit-images h3 {
    margin: 0;
    font-size: 0.82rem;
    font-weight: normal;
    color: var(--text-muted);
  }
  .image-map table {
    width: 100%;
    margin-top: 0.4rem;
    border-collapse: collapse;
    table-layout: fixed;
  }
  .image-map th {
    text-align: left;
    font-size: 0.68rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-muted);
    font-weight: normal;
    padding: 0.2rem 0.4rem;
  }
  .image-map td {
    vertical-align: top;
    padding: 0.25rem 0.4rem;
    border-top: 1px solid var(--border);
  }
  .inputs {
    margin-bottom: 1.25rem;
  }
  .inputs h2 {
    margin-bottom: 0.4rem;
  }
  .uri-list {
    margin: 0;
    padding: 0.6rem 0.9rem;
    list-style: none;
    border: 1px solid var(--border);
    border-radius: 0.5rem;
    background: var(--bg-content);
  }
  .uri-list li {
    font-family: var(--mono);
    font-size: 0.78rem;
    color: var(--text);
    word-break: break-all;
    padding: 0.1rem 0;
  }
  /* The resume command: selectable, with a copy button beside it. */
  .resume {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
  }
  .resume code {
    user-select: all;
  }
  .copy {
    font-size: 0.72rem;
    padding: 0.05rem 0.4rem;
    border: 1px solid var(--border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text-muted);
    cursor: pointer;
  }
  .meta {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
    gap: 0.6rem;
    margin-bottom: 1.5rem;
    padding: 0.75rem 0.9rem;
    border: 1px solid var(--border);
    border-radius: 0.5rem;
    background: var(--bg-content);
  }
  .meta-item {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
  }
  .meta-label {
    font-size: 0.68rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-muted);
  }
  .meta-value {
    font-size: 0.88rem;
    color: var(--text);
  }
  .meta.single {
    grid-template-columns: 1fr;
  }
  .mono {
    font-size: 0.78rem;
    word-break: break-all;
  }
  .tag {
    font-size: 0.68rem;
    padding: 0.05rem 0.4rem;
    border-radius: 0.3rem;
    border: 1px solid var(--border);
    color: var(--text-muted);
    text-transform: lowercase;
  }
  .tag.warn {
    border-color: #f5a62355;
    color: var(--fg-warn);
  }
</style>
