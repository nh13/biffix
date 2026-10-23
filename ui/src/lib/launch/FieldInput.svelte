<script lang="ts">
  // One parameter of the launch form: its label (with unit, required mark and an ⓘ for the author's
  // help), a short description, the control its widget calls for, and its validation message.
  // A value left blank is not sent, so the workflow's own default applies.
  import { uploads } from '$lib/upload/store';
  import { pendingNote } from '$lib/upload/uploads.svelte';
  import type { FormField } from '$lib/workflows/schema';
  import { helpOf, isBlank, labelOf, presetMeaning } from './form';
  import { focusRadio, radioKeyTarget } from '$lib/util/radioGroup';
  import { isBooleanChoice, valueKey } from './validate';
  import FilePicker from './FilePicker.svelte';
  import HelpPanel from './HelpPanel.svelte';

  interface Props {
    field: FormField;
    value: unknown;
    onchange: (value: unknown) => void;
    /** The validation message to show (already filtered for touched/attempted). */
    error?: string | null;
    /** Tagged as hidden by the workflow author (visible because *Show all parameters* is on). */
    hiddenByAuthor?: boolean;
    /** Placeholder when the field has none (e.g. the output location's default). */
    placeholder?: string;
    /** Show the raw param name beside the label (for bioinformaticians, under *Show all*). */
    showName?: boolean;
    ontouch?: () => void;
  }
  let {
    field,
    value,
    onchange,
    error = null,
    hiddenByAuthor = false,
    placeholder,
    showName = false,
    ontouch,
  }: Props = $props();

  const id = $derived(`field-${field.name}`);
  const describedBy = $derived(error ? `${id}-error` : undefined);
  const help = $derived(helpOf(field));
  let helpOpen = $state(false);

  const isBool = $derived(isBooleanChoice(field));
  const isUnset = (v: unknown) => v === undefined || v === null || v === '';
  // The value the workflow will use: the entered one, else the default (text that is only
  // whitespace is not sent, so the default applies).
  const effective = $derived(isBlank(value) ? field.default : value);

  const choices = $derived(field.enum_values ?? []);
  // An enum's chips: the author's presets name its values; otherwise the values themselves.
  const chipChoices = $derived(
    choices.map((c) => ({
      value: c,
      label: field.presets?.find((p) => valueKey(p.value) === c)?.label ?? c,
    })),
  );
  const useChips = $derived(!isBool && choices.length > 0 && choices.length <= 5);

  // Presets on a free-valued field: named chips plus Custom, which reveals the input seeded with
  // the selected preset's value.
  const presetOf = (v: unknown) =>
    isUnset(v) ? undefined : field.presets?.find((p) => valueKey(p.value) === valueKey(v));
  const hasPresets = $derived(!field.enum_values && (field.presets?.length ?? 0) > 0);
  let customOpen = $state(false);
  const selectedPreset = $derived(presetOf(effective));
  const customShown = $derived(hasPresets && (customOpen || !selectedPreset));

  const isPath = $derived(
    field.widget === 's3_file' ||
      field.widget === 's3_dir' ||
      field.widget === 'samplesheet' ||
      field.widget === 'reference_bundle',
  );
  // A param that takes a list of paths (`multiple: true`): one per line, and the picker adds files.
  const isList = $derived(!!field.multiple && isPath);
  const paths = $derived(
    Array.isArray(value)
      ? value.filter((v): v is string => typeof v === 'string')
      : typeof value === 'string' && value
        ? [value]
        : [],
  );
  // A path the user is uploading into: its progress, until S3 has the whole file (the first such
  // of a list).
  const uploading = $derived(
    paths.map((p) => uploads.pendingFor(p.trim())).find((u) => u !== null) ?? null,
  );
  const pathPlaceholder = $derived(
    field.widget === 's3_dir' ? 's3://bucket/folder/' : 's3://bucket/path/file',
  );
  let pickerOpen = $state(false);
  const defaultText = $derived(isUnset(field.default) ? '' : `Default: ${valueKey(field.default)}`);

  function setText(e: Event) {
    onchange((e.currentTarget as HTMLInputElement | HTMLTextAreaElement).value);
  }
  function pickChip(v: string) {
    // Clicking the selected chip of an optional field without a default clears it.
    if (!field.required && isUnset(field.default) && valueKey(value) === v) onchange(undefined);
    else onchange(v);
  }
  function pickPreset(v: unknown) {
    customOpen = false;
    onchange(v);
  }
  function openCustom() {
    ontouch?.();
    customOpen = true;
    // Seed with the selected preset (or default) so Custom starts from a known-good value.
    if (!isUnset(effective)) onchange(valueKey(effective));
  }
  function setList(e: Event) {
    const lines = (e.currentTarget as HTMLTextAreaElement).value.split('\n');
    const list = lines.map((l) => l.trim()).filter((l) => l !== '');
    onchange(list.length > 0 ? list : undefined);
  }
  /** The chips' keys (one Tab stop; arrows, Home and End move the choice); `pick` takes the index. */
  function chipKeydown(e: KeyboardEvent, at: number, n: number, pick: (i: number) => void) {
    const to = radioKeyTarget(e.key, Math.max(0, at), n);
    if (to === null) return;
    e.preventDefault();
    ontouch?.();
    pick(to);
    focusRadio(e, to);
  }
  const range = $derived.by(() => {
    const lo = field.min;
    const hi = field.max;
    if (lo != null && hi != null) return `${lo}–${hi}`;
    if (lo != null) return `≥ ${lo}`;
    if (hi != null) return `≤ ${hi}`;
    return '';
  });
</script>

<div class="field" class:invalid={!!error}>
  <div class="label-row">
    <label for={id} id={`${id}-label`}>
      {labelOf(field)}{#if field.unit}<span class="unit">
          [{field.unit}]</span
        >{/if}{#if field.required}<span class="req" aria-hidden="true">*</span><span
          class="sr-only"
        >
          (required)</span
        >{/if}
    </label>
    {#if help}
      <button
        type="button"
        class="info"
        aria-expanded={helpOpen}
        aria-controls={`${id}-help`}
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
    {#if hiddenByAuthor}<span class="tag">hidden by author</span>{/if}
    {#if showName}<code class="name">{field.name}</code>{/if}
  </div>
  {#if field.description}<p class="desc">{field.description}</p>{/if}
  {#if help && helpOpen}<HelpPanel id={`${id}-help`} {help} />{/if}

  <div class="control">
    {#if isBool}
      {@const on = valueKey(effective) === 'true'}
      <button
        type="button"
        role="switch"
        {id}
        class="switch"
        aria-checked={on}
        aria-labelledby={`${id}-label`}
        aria-invalid={!!error}
        aria-describedby={describedBy}
        onclick={() => {
          ontouch?.();
          onchange(!on);
        }}><span class="knob"></span></button
      ><span class="switch-text">{on ? 'On' : 'Off'}</span>
    {:else if useChips}
      {@const at = chipChoices.findIndex((c) => valueKey(effective) === c.value)}
      <div
        class="chips"
        role="radiogroup"
        aria-labelledby={`${id}-label`}
        aria-invalid={!!error}
        aria-describedby={describedBy}
        {id}
        tabindex="-1"
      >
        {#each chipChoices as c, i}
          {@const selected = i === at}
          <button
            type="button"
            role="radio"
            aria-checked={selected}
            tabindex={selected || (at < 0 && i === 0) ? 0 : -1}
            class:selected
            onclick={() => {
              ontouch?.();
              pickChip(c.value);
            }}
            onkeydown={(e) =>
              chipKeydown(e, at, chipChoices.length, (to) => onchange(chipChoices[to].value))}
            >{c.label}</button
          >
        {/each}
      </div>
    {:else if choices.length > 0}
      <select
        {id}
        aria-invalid={!!error}
        aria-describedby={describedBy}
        value={isUnset(value) ? '' : valueKey(value)}
        onchange={(e) => {
          ontouch?.();
          const v = (e.currentTarget as HTMLSelectElement).value;
          onchange(v === '' ? undefined : v);
        }}
      >
        <option value=""
          >{isUnset(field.default) ? '— not set —' : `Default (${valueKey(field.default)})`}</option
        >
        {#each chipChoices as c}<option value={c.value}>{c.label}</option>{/each}
      </select>
    {:else}
      {#if hasPresets}
        {@const presets = field.presets ?? []}
        {@const at = customShown ? presets.length : presets.findIndex((p) => p === selectedPreset)}
        {@const pickAt = (i: number) =>
          i < presets.length ? pickPreset(presets[i].value) : openCustom()}
        <div
          class="chips"
          role="radiogroup"
          aria-label={`${labelOf(field)} presets`}
          aria-invalid={!!error}
          aria-describedby={describedBy}
        >
          {#each presets as p, i}
            {@const selected = i === at}
            <button
              type="button"
              role="radio"
              aria-checked={selected}
              tabindex={selected || (at < 0 && i === 0) ? 0 : -1}
              class:selected
              onclick={() => {
                ontouch?.();
                pickPreset(p.value);
              }}
              onkeydown={(e) => chipKeydown(e, at, presets.length + 1, pickAt)}>{p.label}</button
            >
          {/each}
          <button
            type="button"
            role="radio"
            aria-checked={customShown}
            tabindex={customShown ? 0 : -1}
            class:selected={customShown}
            onclick={openCustom}
            onkeydown={(e) => chipKeydown(e, at, presets.length + 1, pickAt)}>Custom</button
          >
        </div>
        {#if !customShown && selectedPreset}
          {@const meaning = presetMeaning(field, selectedPreset)}
          {#if meaning}<span class="preset-meaning">{meaning}</span>{/if}
        {/if}
      {/if}
      {#if !hasPresets || customShown}
        {#if isList}
          <textarea
            {id}
            rows="3"
            class="path"
            spellcheck="false"
            value={paths.join('\n')}
            placeholder={field.placeholder ?? `${pathPlaceholder} (one per line)`}
            aria-invalid={!!error}
            aria-describedby={describedBy}
            oninput={setList}
            onblur={() => ontouch?.()}></textarea>
          <button
            type="button"
            class="browse"
            aria-label={`Browse for ${labelOf(field)}`}
            onclick={() => (pickerOpen = true)}>Browse…</button
          >
        {:else if field.multiline}
          <textarea
            {id}
            rows="3"
            value={isUnset(value) ? '' : String(value)}
            placeholder={field.placeholder ?? placeholder ?? defaultText}
            aria-invalid={!!error}
            aria-describedby={describedBy}
            oninput={setText}
            onblur={() => ontouch?.()}></textarea>
        {:else}
          <input
            {id}
            type={field.is_secret ? 'password' : 'text'}
            autocomplete="off"
            spellcheck="false"
            inputmode={field.widget === 'number' ? 'decimal' : undefined}
            class:path={isPath}
            value={isUnset(value) ? '' : valueKey(value)}
            placeholder={field.placeholder ??
              placeholder ??
              (isPath ? pathPlaceholder : defaultText)}
            aria-invalid={!!error}
            aria-describedby={describedBy}
            oninput={setText}
            onblur={() => ontouch?.()}
          />
          {#if field.widget === 'number' && range}<span class="range">{range}</span>{/if}
          {#if isPath}
            <button
              type="button"
              class="browse"
              aria-label={`Browse for ${labelOf(field)}`}
              onclick={() => (pickerOpen = true)}>Browse…</button
            >
          {/if}
        {/if}
      {/if}
    {/if}
  </div>
  {#if uploading}
    <p class="uploading" role="status">
      {pendingNote(uploading)}
    </p>
  {/if}
  {#if error}<p class="error" id={`${id}-error`}>{error}</p>{/if}
</div>

{#if pickerOpen}
  <FilePicker
    mode={field.widget === 's3_dir' ? 'folder' : isList ? 'files' : 'file'}
    label={labelOf(field)}
    suffixes={field.suffixes ?? []}
    current={typeof value === 'string' ? value : (paths.at(-1) ?? null)}
    onpick={(uris) => {
      ontouch?.();
      // A list gains what was picked; a single path is replaced by it.
      onchange(isList ? [...new Set([...paths, ...uris])] : uris[0]);
    }}
    onclose={() => (pickerOpen = false)}
  />
{/if}

<style>
  .field {
    padding: 0.7rem 0;
    border-bottom: 1px solid var(--border);
  }
  .field:last-child {
    border-bottom: none;
  }
  .label-row {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    flex-wrap: wrap;
  }
  label {
    font-size: 0.88rem;
    font-weight: 600;
    color: var(--text);
  }
  .unit {
    margin-left: 0.3em;
    font-weight: 400;
    color: var(--text-muted);
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
    font-size: 0.85rem;
    padding: 0 0.15rem;
  }
  .info:hover,
  .info[aria-expanded='true'] {
    color: var(--accent);
  }
  .tag {
    font-size: 0.66rem;
    padding: 0.02rem 0.4rem;
    border-radius: 0.6rem;
    border: 1px solid var(--border);
    color: var(--text-muted);
  }
  .name {
    font-family: var(--mono);
    font-size: 0.7rem;
    color: var(--text-muted);
    margin-left: auto;
  }
  .desc,
  .preset-meaning {
    margin: 0.2rem 0 0;
    font-size: 0.8rem;
    color: var(--text-muted);
    max-width: 60ch;
  }
  .control {
    margin-top: 0.4rem;
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
  }
  input,
  select,
  textarea {
    box-sizing: border-box;
    max-width: 100%;
    font: inherit;
    font-size: 0.85rem;
    padding: 0.35rem 0.55rem;
    border: 1px solid var(--control-border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text);
    min-width: 12rem;
  }
  input.path,
  textarea {
    width: min(100%, 36rem);
    font-family: var(--mono);
    font-size: 0.8rem;
  }
  input:focus-visible,
  select:focus-visible,
  textarea:focus-visible,
  button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .invalid input,
  .invalid textarea,
  .invalid select {
    border-color: var(--fg-fail);
  }
  .browse {
    font: inherit;
    font-size: 0.8rem;
    padding: 0.33rem 0.7rem;
    border: 1px solid var(--control-border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text);
    cursor: pointer;
  }
  .browse:hover {
    background: var(--hover);
  }
  .range {
    font-size: 0.75rem;
    color: var(--text-muted);
  }
  .chips {
    display: inline-flex;
    flex-wrap: wrap;
    border: 1px solid var(--control-border);
    border-radius: 0.45rem;
    overflow: hidden;
  }
  .chips button {
    font: inherit;
    font-size: 0.8rem;
    padding: 0.3rem 0.7rem;
    border: none;
    border-right: 1px solid var(--control-border);
    background: var(--bg-content);
    color: var(--text);
    cursor: pointer;
  }
  .chips button:last-child {
    border-right: none;
  }
  .chips button:hover {
    background: var(--hover);
  }
  .chips button.selected {
    background: var(--tab-active-bg);
    color: var(--tab-active-fg);
  }
  .switch {
    position: relative;
    width: 2.3rem;
    height: 1.3rem;
    border-radius: 1rem;
    border: none;
    background: var(--gray-400);
    cursor: pointer;
    padding: 0;
  }
  .switch[aria-checked='true'] {
    background: var(--accent);
  }
  .knob {
    position: absolute;
    top: 0.15rem;
    left: 0.15rem;
    width: 1rem;
    height: 1rem;
    border-radius: 50%;
    background: #fff;
    transition: left 0.12s;
  }
  .switch[aria-checked='true'] .knob {
    left: 1.15rem;
  }
  @media (prefers-reduced-motion: reduce) {
    .knob {
      transition: none;
    }
  }
  .switch-text {
    font-size: 0.8rem;
    color: var(--text-muted);
  }
  .uploading {
    margin: 0.3rem 0 0;
    font-size: 0.78rem;
    color: var(--text-muted);
  }
  .error {
    margin: 0.3rem 0 0;
    font-size: 0.78rem;
  }
</style>
