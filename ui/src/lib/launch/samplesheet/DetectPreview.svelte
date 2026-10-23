<script lang="ts">
  // Samples detected from the files just picked, previewed before anything is added: the server's
  // grouping of them, and a naming editor for odd names (the id read from the file name or its
  // folder, automatically, from chosen parts of the name, or by a regular expression).
  import type { DetectRule } from '$lib/generated/samples';
  import { plural } from '$lib/util/format';
  import { focusRadio, radioKeyTarget } from '$lib/util/radioGroup';
  import { baseName } from './detect';
  import type { SampleDetection } from './detection.svelte';

  interface Props {
    detection: SampleDetection;
    /** The param, for ids. */
    name: string;
    /** Add the detected samples to the table. */
    onadd: () => void;
  }
  let { detection: d, name, onadd }: Props = $props();

  const ID_FROM = [
    ['name', 'File name'],
    ['name_and_parent', 'File and parent folder'],
  ] as const;
  const METHODS = [
    ['auto', 'Automatic'],
    ['tokens', 'Choose parts'],
    ['regex', 'Regular expression'],
  ] as const;
  type MethodKind = (typeof METHODS)[number][0];
  const fromAt = $derived(ID_FROM.findIndex(([v]) => d.rule.id_from === v));
  const howAt = $derived(METHODS.findIndex(([k]) => d.rule.method?.kind === k));

  function setMethod(kind: MethodKind) {
    const method: DetectRule['method'] =
      kind === 'auto'
        ? { kind: 'auto' }
        : kind === 'tokens'
          ? { kind: 'tokens', selected: [] }
          : { kind: 'regex', pattern: d.regex };
    d.setRule({ ...d.rule, method });
  }
  /** A chip row's keys: one Tab stop, the arrows (Home, End) move the choice. */
  function radioKeys(e: KeyboardEvent, at: number, n: number, pick: (i: number) => void) {
    const to = radioKeyTarget(e.key, Math.max(0, at), n);
    if (to === null) return;
    e.preventDefault();
    pick(to);
    focusRadio(e, to);
  }
</script>

<section class="preview" aria-labelledby={`preview-${name}`}>
  <h3 id={`preview-${name}`}>Detected samples</h3>
  <!-- A failed detection (a pattern the server refuses, a network error) is said above what is
       on show, so the naming can be fixed and Cancel stays. -->
  {#if d.error}
    <p class="error" role="alert">{d.error}</p>
  {/if}
  {#if d.preview}
    {@const preview = d.preview}
    <p class="summary" aria-live="polite">{d.summary}{d.detecting ? ' · updating…' : ''}</p>
    <div class="scroll">
      <table>
        <thead>
          <tr><th>Sample</th><th>Read 1</th><th>Read 2</th><th>Lanes</th><th>Problems</th></tr>
        </thead>
        <tbody>
          {#each preview.detection.samples as s (s.id)}
            <tr class:flagged={(s.problems?.length ?? 0) > 0}>
              <td class="mono">{s.id}</td>
              <td class="mono">{s.read1.map(baseName).join(', ')}</td>
              <td class="mono">{s.read2.map(baseName).join(', ') || '—'}</td>
              <td>{s.lanes}</td>
              <td>{(s.problems ?? []).join('; ')}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
    {#if d.naming}
      <div class="naming">
        <div class="naming-row">
          <span class="naming-label">Sample id from</span>
          <div class="chips" role="radiogroup" aria-label="Sample id from">
            {#each ID_FROM as [value, text], i}
              <button
                role="radio"
                aria-checked={i === fromAt}
                tabindex={i === Math.max(0, fromAt) ? 0 : -1}
                class:selected={i === fromAt}
                onclick={() => d.setRule({ ...d.rule, id_from: value })}
                onkeydown={(e) =>
                  radioKeys(e, fromAt, ID_FROM.length, (to) =>
                    d.setRule({ ...d.rule, id_from: ID_FROM[to][0] }),
                  )}>{text}</button
              >
            {/each}
          </div>
        </div>
        <div class="naming-row">
          <span class="naming-label">How</span>
          <div class="chips" role="radiogroup" aria-label="How to read the sample id">
            {#each METHODS as [kind, text], i}
              <button
                role="radio"
                aria-checked={i === howAt}
                tabindex={i === Math.max(0, howAt) ? 0 : -1}
                class:selected={i === howAt}
                onclick={() => setMethod(kind)}
                onkeydown={(e) =>
                  radioKeys(e, howAt, METHODS.length, (to) => setMethod(METHODS[to][0]))}
                >{text}</button
              >
            {/each}
          </div>
        </div>
        {#if d.rule.method?.kind === 'auto'}
          <p class="rule-note">
            Automatic reads the common schemes: Illumina <code>P01_S1_L001_R1_001</code>,
            <code>P01_R1</code> and <code>P01_1</code>. Files of one sample from several lanes are
            merged; a <code>.gz</code> copy wins over an uncompressed one. Index reads such as
            <code>_I1_001</code> and files listed twice are left out.
          </p>
        {:else if d.rule.method?.kind === 'tokens'}
          <p class="rule-note">
            Click the parts of <span class="mono">{preview.example}</span> that make up the sample id.
          </p>
          <div class="tokens" role="group" aria-label="Parts of the file name">
            {#each preview.tokens as t, i}
              <button
                class="token"
                class:sep={t.separator}
                aria-pressed={d.selectedTokens.includes(i)}
                onclick={() => d.toggleToken(i)}>{t.text}</button
              >
            {/each}
          </div>
        {:else}
          <label class="regex">
            <span>Pattern (the first group, or the group named <code>sample</code>, is the id)</span
            >
            <input
              class="mono"
              spellcheck="false"
              placeholder="^PRJ9_(P\d+)_"
              bind:value={d.regex}
              oninput={() => d.setRule({ ...d.rule, method: { kind: 'regex', pattern: d.regex } })}
            />
          </label>
        {/if}
        <p class="result" aria-live="polite">
          Resulting sample id for <span class="mono">{preview.example}</span>:
          <strong class="mono">{d.exampleId ?? '(no match)'}</strong>
        </p>
      </div>
    {/if}
    <div class="preview-actions">
      <button
        class="btn btn-compact"
        onclick={() => (d.naming = !d.naming)}
        aria-expanded={d.naming}>{d.naming ? 'Hide naming' : 'Fix naming'}</button
      >
      <span class="spacer"></span>
      <button class="btn btn-compact" onclick={() => d.close()}>Cancel</button>
      <button
        class="btn btn-compact btn-primary"
        onclick={onadd}
        disabled={d.detecting || !!d.error || preview.detection.samples.length === 0}
        >Add {plural(preview.detection.samples.length, 'sample')}</button
      >
    </div>
  {:else if d.error}
    <div class="preview-actions">
      <span class="spacer"></span>
      <button class="btn btn-compact" onclick={() => d.close()}>Cancel</button>
    </div>
  {:else}
    <p class="muted">Reading {d.files.length} file names…</p>
  {/if}
</section>

<style>
  /* The preview box, its table and its actions row are the builder's (SamplesheetBuilder.svelte),
     shared with the column-matching step; these are the naming editor's own. */
  tr.flagged td {
    background: color-mix(in oklch, var(--fg-warn) 10%, transparent);
  }
  .mono {
    font-size: 0.78rem;
  }
  .naming {
    border-top: 1px solid var(--card-border);
    margin-top: 0.6rem;
    padding-top: 0.6rem;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .naming-row {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    flex-wrap: wrap;
  }
  .naming-label {
    font-weight: 600;
    min-width: 7rem;
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
    padding: 0.3rem 0.65rem;
    border: none;
    border-right: 1px solid var(--control-border);
    background: var(--bg-content);
    color: var(--text);
    cursor: pointer;
  }
  .chips button:last-child {
    border-right: none;
  }
  .chips button.selected {
    background: var(--tab-active-bg);
    color: var(--tab-active-fg);
  }
  .tokens {
    display: flex;
    flex-wrap: wrap;
    gap: 0.2rem;
  }
  .token {
    font-family: var(--mono);
    font-size: 0.82rem;
    padding: 0.25rem 0.5rem;
    border: 1px solid var(--control-border);
    border-radius: 0.3rem;
    background: var(--bg-content);
    color: var(--text);
    cursor: pointer;
  }
  .token.sep {
    padding: 0.25rem 0.3rem;
    color: var(--text-muted);
  }
  .token[aria-pressed='true'] {
    background: var(--accent);
    border-color: var(--accent);
    color: var(--bg-content);
  }
  .regex {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .regex input {
    font-size: 0.82rem;
    padding: 0.35rem 0.5rem;
    border: 1px solid var(--control-border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text);
    width: min(100%, 30rem);
  }
  .rule-note,
  .result {
    margin: 0;
    color: var(--text-muted);
  }
  .result strong {
    color: var(--text);
  }
</style>
