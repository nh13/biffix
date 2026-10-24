<script lang="ts">
  // *Share* on the analysis page: the page's own link to copy, and who can open it.
  // No share tokens and no public links: signing in and the project's access decide, as for any
  // page. A disclosure (`Disclosure`): Escape, or a click or focus outside, closes it.
  import { CopiedFlag } from '#lib/util/clipboard.svelte.js';
  import { Disclosure } from '#lib/util/disclosure.svelte.js';
  import { shareSentence } from './analysis';

  let { url, project }: { url: string; project: string | null | undefined } = $props();

  const uid = $props.id();
  const panelId = `${uid}-share`;
  const disclosure = new Disclosure();
  let failed = $state(false);
  const copied = new CopiedFlag();

  async function copy() {
    failed = !(await copied.copy(url, 'link'));
  }
</script>

<svelte:window onpointerdown={disclosure.onWindowPointer} />

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="share"
  bind:this={disclosure.root}
  onkeydown={disclosure.onkeydown}
  onfocusout={disclosure.onfocusout}
>
  <button
    type="button"
    class="btn"
    bind:this={disclosure.button}
    aria-expanded={disclosure.open}
    aria-controls={panelId}
    onclick={() => {
      disclosure.toggle();
      failed = false;
    }}>Share</button
  >
  {#if disclosure.open}
    <div class="panel" id={panelId} role="region" aria-label="Share this analysis">
      <label for={`${panelId}-url`}>Link to this analysis</label>
      <div class="copy-row">
        <input
          id={`${panelId}-url`}
          type="text"
          readonly
          value={url}
          onfocus={(e) => e.currentTarget.select()}
        />
        <button type="button" class="btn" onclick={copy}
          >{copied.is('link') ? 'Copied' : 'Copy link'}</button
        >
      </div>
      {#if failed}
        <p class="error" role="alert">Couldn't copy; select the link and copy it yourself.</p>
      {/if}
      <p class="who">{shareSentence(project)}</p>
      <p class="who" data-testid="share-expert">
        Sending it to a bioinformatician? They can switch to the <strong>Expert</strong> view at the top
        right of the page for logs and task details.
      </p>
    </div>
  {/if}
</div>

<style>
  .share {
    position: relative;
    display: inline-flex;
  }
  .panel {
    position: absolute;
    top: calc(100% + 0.4rem);
    right: 0;
    z-index: 20;
    width: min(24rem, calc(100vw - 2rem));
    box-sizing: border-box;
    padding: 0.8rem 0.9rem;
    border: 1px solid var(--card-border);
    border-radius: 0.6rem;
    background: var(--bg-content);
    box-shadow: 0 8px 24px rgb(0 0 0 / 0.18);
  }
  label {
    display: block;
    margin-bottom: 0.3rem;
    font-size: 0.8rem;
    font-weight: 600;
  }
  .copy-row {
    display: flex;
    gap: 0.4rem;
  }
  input {
    flex: 1;
    min-width: 0;
    padding: 0.35rem 0.5rem;
    border: 1px solid var(--control-border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text);
    font: inherit;
    font-size: 0.8rem;
  }
  .who {
    margin: 0.6rem 0 0;
    font-size: 0.8rem;
    color: var(--text-muted);
  }
  .error {
    margin: 0.4rem 0 0;
    font-size: 0.78rem;
  }
  /* A narrow screen: the header's actions wrap under the title, so the popover opens from the
     left edge of its button rather than off the screen. */
  @media (max-width: 640px) {
    .panel {
      right: auto;
      left: 0;
    }
  }
</style>
