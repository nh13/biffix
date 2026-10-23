<script lang="ts">
  // The reference dropdown: pick an admin-registered bundle that fills the
  // form's reference params, or — when the author allows it — use your own files, which shows those
  // params' own fields in this card instead.
  import type { ReferenceBundle } from '$lib/api';
  import type { FormReferences } from '$lib/workflows/schema';
  import { view } from '$lib/state/userPrefs.svelte';
  import { chosenBundle, fillsOf, REFERENCE_FIELD, type RefChoice } from './referenceChoice';
  import HelpPanel from './HelpPanel.svelte';

  interface Props {
    refs: FormReferences;
    /** The bundles on offer; `null` while loading. */
    offered: ReferenceBundle[] | null;
    loadError: string | null;
    choice: RefChoice;
    onchoice: (choice: RefChoice) => void;
    error: string | null;
    /** Whether any bundle is registered at all (of any kind), once known: `false` says none is. */
    anyRegistered?: boolean | null;
  }
  let { refs, offered, loadError, choice, onchoice, error, anyRegistered = null }: Props = $props();

  const id = `field-${REFERENCE_FIELD}`;
  const chosen = $derived(chosenBundle(choice, offered));
  const fills = $derived(chosen ? fillsOf(chosen, refs) : null);
  const needs = $derived(refs.required.join(' and '));
  const help = $derived(refs.help?.trim() || null);
  let helpOpen = $state(false);
</script>

{#snippet helpButton()}
  {#if help}
    <button
      type="button"
      class="info"
      aria-expanded={helpOpen}
      aria-controls={`${id}-help`}
      title="About this choice"
      onclick={() => (helpOpen = !helpOpen)}
      onkeydown={(e) => {
        if (e.key === 'Escape' && helpOpen) {
          e.stopPropagation();
          helpOpen = false;
        }
      }}>ⓘ<span class="sr-only"> About {refs.label}</span></button
    >
  {/if}
{/snippet}

{#snippet noneOffered()}
  <p class="desc">
    {#if anyRegistered === false}
      No reference genomes registered yet — an admin adds them under
    {:else if needs}
      No registered reference has {needs}; an admin can add one under
    {:else}
      No registered reference fits this pipeline yet; an admin can add one under
    {/if}
    <a href="/settings/references">Settings → References</a>{#if view.mode === 'lab'}{' '}(Expert
      view){/if}.
  </p>
{/snippet}

<div class="field" class:invalid={!!error}>
  {#if choice.custom && refs.custom}
    <div class="label-row">
      <span class="label">{refs.label}</span>
      <span class="muted">· your own files, below</span>
      {@render helpButton()}
    </div>
    {#if help && helpOpen}<HelpPanel id={`${id}-help`} {help} />{/if}
    {#if loadError}
      <p class="error" role="alert">Couldn't load references: {loadError}</p>
    {:else if offered === null || offered.length > 0}
      <button type="button" class="link" onclick={() => onchoice({ bundle: null, custom: false })}
        >Use a registered reference instead</button
      >
    {:else}
      {@render noneOffered()}
    {/if}
  {:else}
    <div class="label-row">
      <label for={id}>{refs.label}<span class="req" aria-hidden="true">*</span></label>
      {@render helpButton()}
    </div>
    {#if help && helpOpen}<HelpPanel id={`${id}-help`} {help} />{/if}
    <div class="control">
      <select
        {id}
        value={choice.bundle ?? ''}
        disabled={offered === null}
        aria-invalid={!!error}
        aria-describedby={error ? `${id}-error` : undefined}
        onchange={(e) => {
          const v = (e.currentTarget as HTMLSelectElement).value;
          onchoice({ bundle: v || null, custom: false });
        }}
      >
        <option value="">{offered === null ? 'Loading…' : 'Choose a reference…'}</option>
        {#each offered ?? [] as b (b.id)}
          <option value={b.id} title={`${b.id} · ${Object.keys(b.uris).join(', ')}`}
            >{b.display_name}</option
          >
        {/each}
        {#if choice.bundle && offered && !chosen}
          <option value={choice.bundle}>{choice.bundle} (not available)</option>
        {/if}
      </select>
      {#if refs.custom}
        <button type="button" class="link" onclick={() => onchoice({ bundle: null, custom: true })}
          >Use my own files…</button
        >
      {/if}
    </div>
    {#if loadError}
      <p class="error" role="alert">Couldn't load references: {loadError}</p>
    {:else if offered && offered.length === 0}
      {@render noneOffered()}
    {/if}
    {#if fills}
      <p class="desc fills">
        Fills {#each fills.fills as f, i}<span title={f.uri}><code>{f.key}</code></span>{i <
          fills.fills.length - 1
            ? ', '
            : ''}{/each}{#if fills.lacks.length > 0}
          · no {fills.lacks.join(', ')} in this reference; give your own, or the workflow's default applies{/if}
      </p>
      {#if chosen?.notes}<p class="desc">{chosen.notes}</p>{/if}
    {/if}
  {/if}
  {#if error}<p class="error" id={`${id}-error`}>{error}</p>{/if}
</div>

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
  label,
  .label {
    font-size: 0.88rem;
    font-weight: 600;
    color: var(--text);
  }
  .req {
    color: var(--fg-fail);
    margin-left: 0.1rem;
  }
  .muted {
    font-size: 0.8rem;
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
  .control {
    margin-top: 0.4rem;
    display: flex;
    align-items: center;
    gap: 0.75rem;
    flex-wrap: wrap;
  }
  select {
    box-sizing: border-box;
    max-width: 100%;
    min-width: 16rem;
    font: inherit;
    font-size: 0.85rem;
    padding: 0.35rem 0.55rem;
    border: 1px solid var(--control-border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text);
  }
  select:focus-visible,
  button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .invalid select {
    border-color: var(--fg-fail);
  }
  /* Not underlined: a control beside the field, not a link in a sentence. */
  .link {
    text-decoration: none;
    font-size: 0.82rem;
  }
  .desc {
    margin: 0.35rem 0 0;
    font-size: 0.8rem;
    color: var(--text-muted);
    max-width: 60ch;
  }
  .desc a {
    color: var(--accent);
  }
  .fills code {
    font-family: var(--mono);
    font-size: 0.78rem;
  }
  .error {
    margin: 0.3rem 0 0;
    font-size: 0.78rem;
  }
</style>
