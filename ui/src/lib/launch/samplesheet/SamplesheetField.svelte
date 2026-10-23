<script lang="ts">
  // A samplesheet param with declared columns: built as rows (the default, which biffix writes as
  // the sheet file), or given as the path of a sheet that already exists. Only rendered for a field
  // with a `samplesheet`; the template's `{:else if field.samplesheet}` just narrows its type.
  import type { FormField } from '$lib/workflows/schema';
  import FieldInput from '../FieldInput.svelte';
  import HelpPanel from '../HelpPanel.svelte';
  import SamplesheetBuilder from './SamplesheetBuilder.svelte';
  import { helpOf, labelOf } from '../form';
  import type { SheetRow } from './sheet';

  interface Props {
    field: FormField;
    mode: 'rows' | 'path';
    onmode: (mode: 'rows' | 'path') => void;
    rows: SheetRow[];
    onrows: (rows: SheetRow[]) => void;
    value: unknown;
    onvalue: (value: unknown) => void;
    /** The field-level message (e.g. required), when it should show. */
    error?: string | null;
    showErrors?: boolean;
    ontouch?: () => void;
    /** A sample whose row the builder points out on arrival. */
    highlight?: string | null;
    /** Where a CSV import's column mapping is remembered. */
    memoryKey?: string | null;
  }
  let {
    field,
    mode,
    onmode,
    rows,
    onrows,
    value,
    onvalue,
    error = null,
    showErrors = false,
    ontouch,
    highlight = null,
    memoryKey = null,
  }: Props = $props();
  const help = $derived(helpOf(field));
  let helpOpen = $state(false);
</script>

{#if mode === 'path'}
  <FieldInput {field} {value} onchange={onvalue} {error} {ontouch} />
  <button class="switch-mode" onclick={() => onmode('rows')}
    >Build the samplesheet from files instead</button
  >
{:else if field.samplesheet}
  <div class="sheet-field">
    <div class="label-row">
      <span class="label" id={`${field.name}-label`}
        >{labelOf(field)}{#if field.required}<span class="req" aria-hidden="true">*</span
          >{/if}</span
      >
      {#if help}
        <button
          class="info"
          aria-expanded={helpOpen}
          aria-controls={`field-${field.name}-help`}
          title="About this parameter"
          onclick={() => (helpOpen = !helpOpen)}
          onkeydown={(e) => {
            if (e.key === 'Escape' && helpOpen) {
              e.stopPropagation();
              helpOpen = false;
            }
          }}>ⓘ<span class="sr-only"> About {labelOf(field)}</span></button
        >
      {/if}
      <button class="switch-mode" onclick={() => onmode('path')}
        >Use an existing samplesheet file</button
      >
    </div>
    {#if field.description}<p class="desc">{field.description}</p>{/if}
    {#if help && helpOpen}<HelpPanel id={`field-${field.name}-help`} {help} />{/if}
    <SamplesheetBuilder
      name={field.name}
      label={labelOf(field)}
      spec={field.samplesheet}
      {rows}
      onchange={(next) => {
        ontouch?.();
        onrows(next);
      }}
      {showErrors}
      {highlight}
      {memoryKey}
    />
    {#if error}<p class="error">{error}</p>{/if}
  </div>
{/if}

<style>
  .sheet-field {
    padding: 0.7rem 0;
  }
  .label-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
  }
  .label {
    font-size: 0.88rem;
    font-weight: 600;
  }
  .req {
    color: var(--fg-fail);
    margin-left: 0.1rem;
  }
  .info {
    border: none;
    background: none;
    color: var(--text-muted);
    cursor: pointer;
  }
  .switch-mode {
    margin-left: auto;
    border: none;
    background: none;
    padding: 0;
    color: var(--accent);
    cursor: pointer;
    font: inherit;
    font-size: 0.8rem;
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  .desc {
    margin: 0.2rem 0 0;
    font-size: 0.8rem;
    color: var(--text-muted);
    max-width: 60ch;
  }
  .error {
    margin: 0.3rem 0 0;
    font-size: 0.78rem;
  }
  button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
</style>
