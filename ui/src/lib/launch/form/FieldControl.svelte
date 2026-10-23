<script lang="ts">
  // One parameter's control in the launch form: a samplesheet (rows, or an existing sheet's path),
  // else the field's own input.
  import type { FormField } from '$lib/workflows/schema';
  import FieldInput from '../FieldInput.svelte';
  import SamplesheetField from '../samplesheet/SamplesheetField.svelte';
  import { launchForm } from './launchForm.svelte';

  let { field: f }: { field: FormField } = $props();
  const form = launchForm();
</script>

{#if f.widget === 'samplesheet' && f.samplesheet}
  <SamplesheetField
    field={f}
    mode={form.sheetModes[f.name] ?? 'rows'}
    onmode={(m) => form.setSheetMode(f.name, m)}
    rows={form.tables[f.name] ?? []}
    onrows={(rows) => form.setRows(f.name, rows)}
    value={form.values[f.name]}
    onvalue={(v) => form.setValue(f.name, v)}
    ontouch={() => form.touch(f.name)}
    error={form.errors.some((e) => e.field === f.name) ? form.errorFor(f.name) : null}
    showErrors={form.attempted}
    highlight={form.highlightSample}
    memoryKey={form.preview ? null : `${form.detail.name}:${f.name}`}
  />
{:else}
  <FieldInput
    field={f}
    value={form.values[f.name]}
    onchange={(v) => form.setValue(f.name, v)}
    ontouch={() => form.touch(f.name)}
    error={form.errorFor(f.name)}
    hiddenByAuthor={f.hidden}
    showName={form.shownAll}
  />
{/if}
