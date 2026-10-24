<script lang="ts">
  // *Inputs used*: the samples, the reference, the options that differ from the
  // pipeline's standard settings, the name and where the results are (read-only), and the params
  // file a bioinformatician can relaunch it from.
  import type { RunDetail, WorkflowDetail } from '#lib/api/index.js';
  import { saveBlob } from '#lib/util/download.js';
  import { plural } from '#lib/util/format.js';
  import { paramsYaml } from '#lib/launch/form.js';
  import { asRetention } from '#lib/launch/retention.js';
  import { renderSheet } from '#lib/launch/samplesheet/sheet.js';
  import { parseSchema } from '#lib/workflows/schema.js';
  import {
    allSettings,
    s3Uris,
    settingCount,
    sharesOriginFolder,
    sheetFile,
    usedFiles,
    usedOptions,
    usedSheets,
    type UsedSheet,
  } from './inputs';
  import S3Location from '#lib/data/S3Location.svelte';
  import { DataLinks } from '#lib/data/dataLinks.svelte.js';
  import { runVersionHref, runVersionLabel } from '#lib/runs/runVersion.js';
  import { pipelineTitle, runName } from '#lib/runs/runName.js';
  import { asRecord } from '#lib/runs/runFacts.js';

  interface Props {
    run: RunDetail;
    /** The run's workflow version (its form), once loaded. */
    version: WorkflowDetail | null;
    /** Why the version could not be loaded, if it could not. */
    versionError?: string | null;
    /** The analysis a resumed one continues, once loaded. */
    origin?: RunDetail | null;
  }
  let { run, version, versionError = null, origin = null }: Props = $props();
  // A resumed analysis writes into the folder of the one it continues.
  const sharedFolder = $derived(sharesOriginFolder(run, origin));

  const schema = $derived(version ? parseSchema(version.form_schema) : null);
  const sheets = $derived(schema ? usedSheets(run.input_tables, schema) : []);
  const bundle = $derived(run.reference_bundle?.id ?? null);
  const usedBundle = $derived(bundle !== null);
  const options = $derived(
    schema ? usedOptions(run.params, schema, run.engine ?? '', usedBundle) : null,
  );
  const sampleTotal = $derived(run.samples?.total ?? 0);
  const versionHref = $derived(runVersionHref(run));
  // Everything else it ran with: the files it was given, and every setting (behind a disclosure).
  const sheetParams = $derived(new Set(sheets.map((s) => s.param)));
  const files = $derived(
    schema ? usedFiles(run.params, schema, run.engine ?? '', { usedBundle, sheetParams }) : [],
  );
  const settings = $derived(
    schema ? allSettings(run.params, schema, { usedBundle, sheetParams }) : [],
  );
  const settingTotal = $derived(settingCount(settings));

  // Files a samplesheet's cells name, linked into the Data page.
  const dataLinks = new DataLinks();
  $effect(() => {
    dataLinks.load(run.project);
  });

  function downloadSheet(sheet: UsedSheet) {
    const file = sheetFile(sheet.param, sheet.spec.format);
    saveBlob(new Blob([renderSheet(sheet.spec, sheet.rows)], { type: file.type }), file.name);
  }

  function downloadParams() {
    const yaml = paramsYaml(run.workflow ?? 'VERSION', asRecord(run.params), {
      reference: bundle,
      retention: run.engine === 'nextflow' ? asRetention(run.retention) : null,
    });
    saveBlob(new Blob([yaml], { type: 'application/x-yaml' }), 'params.yaml');
  }
</script>

<section id="inputs" aria-labelledby="inputs-h">
  <h2 id="inputs-h">Inputs used</h2>
  <dl>
    <dt>Samples</dt>
    <dd>
      {#if sheets.length}
        {#each sheets as sheet (sheet.param)}
          <div>
            {sheet.count}
            {sheet.count === 1
              ? 'sample'
              : 'samples'}{#if sheet.first.length}{' · '}{sheet.first.join(
                ', ',
              )}{#if sheet.more > 0}{` and ${sheet.more} more`}{/if}{/if}
            ·
            <button class="link" onclick={() => downloadSheet(sheet)}
              >Download the samplesheet<span class="sr-only"> ({sheet.param})</span></button
            >
            <details class="more" data-testid="samplesheet-table">
              <summary>Show all {plural(sheet.rows.length, 'row')} of the samplesheet</summary>
              <!-- Focusable so a keyboard can scroll a sheet wider or longer than the page. -->
              <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
              <div
                class="sheet-scroll"
                role="region"
                aria-label={`Samplesheet ${sheet.param}`}
                tabindex="0"
              >
                <table class="sheet">
                  <thead>
                    <tr>
                      {#each sheet.spec.columns as c (c.name)}<th scope="col">{c.name}</th>{/each}
                    </tr>
                  </thead>
                  <tbody>
                    {#each sheet.rows as row, i (i)}
                      <tr>
                        {#each sheet.spec.columns as c (c.name)}
                          {@const cell = row[c.name] ?? ''}
                          {@const isUri = s3Uris(cell).length > 0}
                          {@const href = isUri ? dataLinks.href(cell.trim()) : null}
                          <td class:mono={isUri}
                            >{#if href}<a {href} aria-label={`Open ${cell} in Data`}>{cell}</a
                              >{:else}{cell}{/if}</td
                          >
                        {/each}
                      </tr>
                    {/each}
                  </tbody>
                </table>
              </div>
            </details>
          </div>
        {/each}
      {:else if sampleTotal > 0}
        {plural(sampleTotal, 'sample')}{run.samples?.declared ? '' : ' seen'}
      {:else}
        <span class="muted">Given as files in its settings</span>
      {/if}
    </dd>
    {#if files.length}
      <dt>Input files</dt>
      <dd>
        <ul class="files" data-testid="input-files">
          {#each files as f (f.name)}
            <li>
              <span class="label">{f.label}</span>:
              {#each f.uris as uri (uri)}<span class="uri-line"
                  ><S3Location {uri} project={run.project} what={f.label} /></span
                >{/each}
            </li>
          {/each}
        </ul>
      </dd>
    {/if}
    <dt>Reference</dt>
    <dd>{bundle ?? 'Its own reference files'}</dd>
    <dt>Options</dt>
    <dd>
      {#if options === null}
        {#if versionError}<span class="error">Couldn't load the pipeline's settings.</span
          >{:else}<span class="muted">Loading…</span>{/if}
      {:else if options.length === 0}
        The pipeline's standard settings
      {:else}
        <ul class="options">
          {#each options as o (o.name)}<li>
              <span class="label">{o.label}</span>: {o.value}
            </li>{/each}
        </ul>
        <span class="muted">{plural(options.length, 'setting')} changed from the standard ones</span
        >
      {/if}
      {#if settingTotal > 0}
        <details class="more" data-testid="all-settings">
          <summary>Show all {plural(settingTotal, 'setting')}</summary>
          {#each settings as section (section.id)}
            <div class="setting-section">
              <h3>{section.title}</h3>
              <dl class="settings">
                {#each section.rows as s (s.name)}
                  <dt>
                    {s.label}{#if s.changed && !s.fromReference}<span class="chip changed"
                        >changed</span
                      >{/if}{#if s.fromReference}<span class="chip">from the reference</span>{/if}
                    {#if s.description}<span class="desc">{s.description}</span>{/if}
                  </dt>
                  <dd>
                    {#if s.uris.length}
                      {#each s.uris as uri (uri)}<span class="uri-line"
                          ><S3Location {uri} project={run.project} what={s.label} /></span
                        >{/each}
                    {:else}
                      <span class:muted={!s.changed}>{s.value}</span>
                    {/if}
                  </dd>
                {/each}
              </dl>
            </div>
          {/each}
        </details>
      {/if}
    </dd>
    <dt>Pipeline</dt>
    <dd>
      {#if versionHref}<a
          class="version"
          href={versionHref}
          title={`Pipeline version ${run.workflow}`}
          data-testid="inputs-version"
          >{pipelineTitle(run) ?? 'Pipeline'}{' · '}{runVersionLabel(run)}</a
        >{:else}Unknown pipeline{/if}
    </dd>
    <dt>Analysis name</dt>
    <dd>{runName(run)}</dd>
    <dt>Where results are</dt>
    <dd>
      {#if sharedFolder}<span class="shared" data-testid="shared-folder"
          >The results folder of the analysis it continues, <a
            href={`/runs/${encodeURIComponent(run.resumed_from ?? '')}`}
            >{origin ? runName(origin) : run.resumed_from}</a
          >: the steps it reused are already there, and the new ones were added.</span
        >{/if}
      {#if run.output_prefix}<S3Location
          uri={run.output_prefix}
          project={run.project}
          what="the results folder"
          testid="results-folder"
        />{:else}—{/if}
    </dd>
  </dl>
  <p class="bioinf">
    For a bioinformatician: <button class="link" onclick={downloadParams}>params YAML</button>
  </p>
</section>

<style>
  section {
    margin-bottom: 1.5rem;
  }
  h2 {
    margin: 0 0 0.6rem;
  }
  dl {
    display: grid;
    grid-template-columns: minmax(7rem, max-content) minmax(0, 1fr);
    gap: 0.35rem 1.2rem;
    margin: 0;
    font-size: 0.86rem;
  }
  dt {
    color: var(--text-muted);
  }
  /* A phone stacks each label over its value, so a value (a log) keeps the width. */
  @media (max-width: 640px) {
    dl {
      grid-template-columns: minmax(0, 1fr);
      gap: 0.1rem;
    }
    dd + dt {
      margin-top: 0.4rem;
    }
  }
  dd {
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .options {
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .label {
    font-weight: 600;
  }
  .shared {
    display: block;
    margin-bottom: 0.2rem;
    color: var(--text-muted);
  }
  .shared a {
    color: var(--accent);
    text-decoration: underline;
  }
  .files {
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .uri-line {
    display: block;
  }
  .more {
    margin-top: 0.35rem;
  }
  .more summary {
    color: var(--accent);
    cursor: pointer;
    width: fit-content;
  }
  .more summary:focus-visible,
  .sheet-scroll:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .sheet-scroll {
    max-height: 24rem;
    overflow: auto;
    margin-top: 0.4rem;
    border: 1px solid var(--border);
    border-radius: 0.45rem;
  }
  .sheet {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.8rem;
  }
  .sheet th,
  .sheet td {
    padding: 0.3rem 0.55rem;
    border-bottom: 1px solid var(--border);
    text-align: left;
    white-space: nowrap;
  }
  .sheet th {
    position: sticky;
    top: 0;
    background: var(--bg-content);
    color: var(--text-muted);
    font-weight: 600;
  }
  .sheet a {
    color: var(--accent);
  }
  .setting-section {
    margin-top: 0.7rem;
  }
  .setting-section h3 {
    margin: 0 0 0.35rem;
    font-size: 0.85rem;
  }
  .settings {
    grid-template-columns: minmax(0, 16rem) minmax(0, 1fr);
    gap: 0.4rem 1.2rem;
  }
  @media (max-width: 640px) {
    .settings {
      grid-template-columns: minmax(0, 1fr);
      gap: 0.1rem;
    }
  }
  .settings dt {
    color: var(--text);
    font-weight: 500;
  }
  .desc {
    display: block;
    font-size: 0.75rem;
    font-weight: normal;
    color: var(--text-muted);
  }
  /* Smaller than the shared chip: it trails a setting's name inside the list. */
  .chip {
    margin-left: 0.4rem;
    padding: 0 0.4rem;
    font-size: 0.68rem;
    font-weight: normal;
  }
  .chip.changed {
    border-color: var(--accent);
    color: var(--accent);
  }
  .bioinf {
    margin: 0.7rem 0 0;
    font-size: 0.8rem;
    color: var(--text-muted);
  }
</style>
