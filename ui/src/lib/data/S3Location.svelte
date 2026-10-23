<script lang="ts">
  // An S3 URI a run names (its output prefix): a link into the Data page when the Data page can
  // show it (else the URI as text), a copy button, and (Expert) the S3 console.
  import { CopiedFlag } from '$lib/util/clipboard.svelte';
  import { s3ConsoleUrl } from '$lib/util/links';
  import { deploy } from '$lib/state/deploy.svelte';
  import { DataLinks } from './dataLinks.svelte';

  interface Props {
    uri: string;
    /** The project whose data sources show it (`null`: the default project). */
    project?: string | null;
    /** What it is, for the buttons' names ("the results folder"). */
    what: string;
    /** Also link the S3 console (requires AWS access: Expert view only). */
    aws?: boolean;
    testid?: string;
  }
  let { uri, project = null, what, aws = false, testid }: Props = $props();

  const links = new DataLinks();
  $effect(() => {
    links.load(project);
  });
  const href = $derived(links.href(uri));
  const consoleUrl = $derived(aws ? s3ConsoleUrl(uri, deploy.region) : null);
  const copied = new CopiedFlag();
</script>

<span class="s3-location" data-testid={testid}>
  <!-- The link's purpose is in its name (with the URI it shows), not only in a tooltip. -->
  {#if href}<a
      class="uri"
      {href}
      aria-label={`Open ${uri} in Data`}
      title={`Open ${what} in the Data browser`}>{uri}</a
    >{:else}<span class="uri">{uri}</span>{/if}
  <button class="copy" onclick={() => copied.copy(uri)}
    >{copied.is(uri) ? 'copied' : 'copy'}<span class="sr-only"> the URI of {what}</span></button
  >
  {#if consoleUrl}<a
      class="aws"
      href={consoleUrl}
      target="_blank"
      rel="noreferrer"
      title="Requires AWS access">AWS ↗<span class="sr-only"> S3 console: {what}</span></a
    >{/if}
</span>

<style>
  .s3-location {
    display: inline-flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.2rem 0.5rem;
    min-width: 0;
  }
  .uri {
    font-family: var(--mono);
    overflow-wrap: anywhere;
  }
  a.uri {
    color: var(--accent);
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
  .aws {
    font-size: 0.75rem;
    color: var(--text-muted);
  }
</style>
