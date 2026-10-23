<script lang="ts">
  // The launch wizard's steps (Pick a pipeline · Add samples · Reference & options · Review & launch):
  // the current one marked, done ones ticked, and the ones that can be reached as links.
  import { plainClick, WIZARD_STEPS, type WizardStep } from './wizard';

  interface Props {
    current: WizardStep;
    /** Where a step links to, or `null` when it can't be reached from here. */
    hrefFor?: (n: WizardStep) => string | null;
    /** Whether a step is done (ticked). */
    done?: (n: WizardStep) => boolean;
    /** Called instead of following a step's link (a plain click; a new-tab click still opens it). */
    onnav?: (n: WizardStep) => void;
  }
  let { current, hrefFor = () => null, done = () => false, onnav }: Props = $props();

  function click(e: MouseEvent, n: WizardStep) {
    if (!onnav || !plainClick(e)) return;
    e.preventDefault();
    onnav(n);
  }
</script>

{#snippet stepText(s: (typeof WIZARD_STEPS)[number], ticked: boolean)}
  <span class="num" aria-hidden="true">{ticked ? '✓' : s.n}</span>
  <span class="label">{s.label}</span>
  {#if ticked}<span class="sr-only"> (done)</span>{/if}
{/snippet}

<nav class="stepper" aria-label="Steps">
  <ol>
    {#each WIZARD_STEPS as s (s.n)}
      {@const href = s.n === current ? null : hrefFor(s.n)}
      {@const ticked = s.n !== current && done(s.n)}
      <li class:current={s.n === current} class:done={ticked}>
        {#if href}
          <a {href} onclick={(e) => click(e, s.n)}>
            {@render stepText(s, ticked)}
          </a>
        {:else}
          <span class="step" aria-current={s.n === current ? 'step' : undefined}>
            {@render stepText(s, ticked)}
            {#if s.n !== current}<span class="sr-only">
                (finish the steps before it first)</span
              >{/if}
          </span>
        {/if}
      </li>
    {/each}
  </ol>
</nav>

<style>
  .stepper ol {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    margin: 0 0 1rem;
    padding: 0;
    list-style: none;
    border: 1px solid var(--card-border);
    border-radius: 0.6rem;
    overflow: hidden;
    background: var(--hover);
  }
  li + li {
    border-left: 1px solid var(--card-border);
  }
  li.current {
    background: var(--bg-content);
  }
  a,
  .step {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.6rem 0.8rem;
    font-size: 0.85rem;
    color: var(--text-muted);
    text-decoration: none;
    min-width: 0;
  }
  a {
    color: var(--text);
  }
  a:hover {
    background: var(--bg-content);
  }
  a:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  li.current .step {
    color: var(--text);
    font-weight: 600;
  }
  .num {
    flex: none;
    display: inline-grid;
    place-items: center;
    width: 1.35rem;
    height: 1.35rem;
    border-radius: 50%;
    border: 1px solid var(--control-border);
    font-size: 0.72rem;
    background: var(--bg-content);
  }
  li.current .num {
    background: var(--tab-active-bg);
    color: var(--tab-active-fg);
    border-color: var(--tab-active-bg);
  }
  li.done .num {
    color: var(--fg-ok);
    border-color: currentColor;
  }
  .label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* On a phone only the current step keeps its name; the others show their number (their names
     stay for screen readers). */
  @media (max-width: 640px) {
    li:not(.current) .label {
      position: absolute;
      width: 1px;
      height: 1px;
      overflow: hidden;
      clip: rect(0 0 0 0);
      white-space: nowrap;
    }
    .stepper ol {
      display: flex;
    }
    li {
      flex: none;
    }
    li.current {
      flex: 1;
      min-width: 0;
    }
    li:not(.current) a,
    li:not(.current) .step {
      justify-content: center;
    }
  }
</style>
