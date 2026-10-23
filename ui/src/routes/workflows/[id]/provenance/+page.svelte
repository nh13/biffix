<script lang="ts">
  import { sourceLinks } from '$lib/workflows/source';
  import { ecrImageUrl } from '$lib/util/links';
  import { getWorkflowContext } from '$lib/workflows/workflowContext.svelte';
  import { workflowHref } from '$lib/workflows/workflowNav';
  import { archMark, imageLabel, isBuilt, shortDigest } from '$lib/workflows/readiness';
  import { CopiedFlag } from '$lib/util/clipboard.svelte';

  const wf = getWorkflowContext();

  const links = $derived(wf.detail ? sourceLinks(wf.detail.source, wf.detail.git_sha) : null);
  const ecr = $derived(
    wf.detail
      ? ecrImageUrl(
          wf.detail.image_registry_id ?? null,
          wf.detail.image_repository ?? null,
          wf.detail.image_digest,
          wf.detail.image_region ?? null,
        )
      : null,
  );
  const copied = new CopiedFlag();
</script>

{#if wf.detail}
  {@const detail = wf.detail}
  <section class="prov">
    <h3>Source</h3>
    {#if links}
      <div class="prov-row">
        <span class="k">Location</span><span class="v mono">{links.label}</span>
      </div>
      <div class="prov-links">
        {#if links.repo}<a href={links.repo} target="_blank" rel="noopener">Repository ↗</a>{/if}
        {#if links.commit}<a href={links.commit} target="_blank" rel="noopener">Commit ↗</a>{/if}
        {#if links.readme}<a href={links.readme} target="_blank" rel="noopener">README ↗</a>{/if}
        {#if links.s3}<a href={links.s3} target="_blank" rel="noopener">S3 console ↗</a>{/if}
      </div>
    {:else}
      <p class="muted">No source recorded.</p>
    {/if}
    {#if detail.registration_inputs?.reregistered_from}
      {@const from = detail.registration_inputs.reregistered_from}
      <div class="prov-row" data-testid="reregistered-from">
        <span class="k">Re-registered from</span>
        <a class="v mono" href={workflowHref(from)}>{from}</a>
      </div>
    {/if}
    {#if detail.registration_inputs?.overrides?.engine_version}
      <div class="prov-row">
        <span class="k">Engine release given</span>
        <span class="v">{detail.registration_inputs.overrides.engine_version}</span>
      </div>
    {/if}

    {#if detail.runtime}
      {@const rt = detail.runtime}
      <h3>Engine</h3>
      <div class="prov-row">
        <span class="k">Release</span><span class="v">{detail.engine} {detail.engine_version}</span>
      </div>
      <div class="prov-row">
        <span class="k">Runtime</span><span class="v mono">{rt.key.slice(0, 12)}</span>
        <span class="state {rt.state}">{rt.state}</span>
      </div>
      {#if rt.coordinator_image}<div class="prov-row">
          <span class="k">Coordinator</span><span class="v mono digest"
            >{shortDigest(rt.coordinator_image)}</span
          >
          <button
            class="copy"
            onclick={() => copied.copy(rt.coordinator_image ?? '', 'coordinator')}
            >{copied.is('coordinator') ? 'copied' : 'copy'}</button
          >
        </div>{/if}
      {#if rt.reason}<div class="prov-row">
          <span class="k">Build</span><span class="v">{rt.reason}</span>
        </div>{/if}
      {#if rt.logs_url}<div class="prov-links">
          <a href={rt.logs_url} target="_blank" rel="noopener">Build log ↗</a>
        </div>{/if}
    {/if}

    {#if (detail.images ?? []).length > 0}
      <h3>Images</h3>
      <table class="images" data-testid="images-table">
        <thead>
          <tr><th>Name</th><th>Declared</th><th>State</th><th>Digest</th><th>Archs</th></tr>
        </thead>
        <tbody>
          {#each detail.images as image (image.name)}
            <tr>
              <td>
                {image.name}
              </td>
              <td class="mono ref">
                {#if isBuilt(image)}
                  <span class="kind" data-testid="image-kind">{image.kind}</span>
                  {image.source_ref.slice(image.source_ref.indexOf(':') + 1)}
                {:else}{image.declared_ref}{/if}
              </td>
              <td>
                <span class="state {image.state}">{imageLabel(image)}</span>
                {#if image.archived}<span class="archived" title="archived byte for byte">⛁</span
                  >{/if}
                {#if (image.arch_builds ?? []).length > 0}
                  <div class="arch-builds" data-testid="arch-builds">
                    {#each image.arch_builds ?? [] as b (b.arch)}
                      <span class="arch {b.state}" title={b.reason ?? b.state}
                        >{b.arch}
                        {#if b.logs_url}<a href={b.logs_url} target="_blank" rel="noopener"
                            >{archMark(b.state)}</a
                          >{:else}{archMark(b.state)}{/if}</span
                      >
                    {/each}
                  </div>
                {/if}
              </td>
              <td class="mono">
                {shortDigest(isBuilt(image) ? image.image : image.source_ref)}
                {#if image.image || !isBuilt(image)}<button
                    class="copy"
                    onclick={() =>
                      copied.copy(image.image ?? image.source_ref, `image:${image.name}`)}
                    >{copied.is(`image:${image.name}`) ? 'copied' : 'copy'}</button
                  >{/if}
                {#if image.derived_state}
                  <div class="derived" data-testid="derived-image">
                    tasks run {image.derived_image ? shortDigest(image.derived_image) : '--'}
                    <span class="muted">(+ {detail.engine} {detail.engine_version} runtime)</span>
                    {#if image.derived_image}<button
                        class="copy"
                        onclick={() =>
                          copied.copy(image.derived_image ?? '', `derived:${image.name}`)}
                        >{copied.is(`derived:${image.name}`) ? 'copied' : 'copy'}</button
                      >{/if}
                  </div>
                {/if}
              </td>
              <td>{(image.archs ?? []).join(', ') || '--'}</td>
            </tr>
            {#if image.generated_dockerfile}
              <tr class="generated">
                <td></td>
                <td colspan="4">
                  <details>
                    <summary>Generated Dockerfile</summary>
                    <pre>{image.generated_dockerfile}</pre>
                  </details>
                </td>
              </tr>
            {/if}
            {#if image.reason}
              <tr class="reason">
                <td></td>
                <td colspan="4">
                  {image.reason}
                  {#if image.logs_url}<a href={image.logs_url} target="_blank" rel="noopener"
                      >log ↗</a
                    >{/if}
                </td>
              </tr>
            {/if}
          {/each}
        </tbody>
      </table>
    {:else}
      <h3>Image</h3>
      {#if detail.image_repository}<div class="prov-row">
          <span class="k">Repository</span><span class="v mono">{detail.image_repository}</span>
        </div>{/if}
      {#if detail.image_region}<div class="prov-row">
          <span class="k">Region</span><span class="v mono">{detail.image_region}</span>
        </div>{/if}
      <div class="prov-row">
        <span class="k">Digest</span>
        <span class="v mono digest">{detail.image_digest}</span>
        <button class="copy" onclick={() => copied.copy(detail.image_digest, 'digest')}
          >{copied.is('digest') ? 'copied' : 'copy'}</button
        >
      </div>
      {#if ecr}<div class="prov-links">
          <a href={ecr} target="_blank" rel="noopener">ECR console ↗</a>
        </div>{/if}
    {/if}

    <h3>Execution</h3>
    <div class="prov-row">
      <span class="k">Entrypoint</span><span class="v mono">{detail.entrypoint}</span>
    </div>
    {#if detail.target}<div class="prov-row">
        <span class="k">Target</span><span class="v mono">{detail.target}</span>
      </div>{/if}
    <div class="prov-row">
      <span class="k">Queues</span><span class="v"
        >{(detail.queue_classes ?? []).join(', ') || '--'}</span
      >
    </div>

    <h3>Frozen id</h3>
    <div class="prov-row">
      <span class="v mono digest">{detail.id}</span>
      <button class="copy" onclick={() => copied.copy(detail.id, 'id')}
        >{copied.is('id') ? 'copied' : 'copy'}</button
      >
    </div>
  </section>
{/if}

<style>
  .prov h3 {
    font-size: 0.78rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-muted);
    margin: 1rem 0 0.35rem;
  }
  .prov-row {
    display: flex;
    align-items: baseline;
    gap: 0.6rem;
    padding: 0.15rem 0;
  }
  .prov-row .k {
    min-width: 6.5rem;
    color: var(--text-muted);
    font-size: 0.78rem;
  }
  .prov-row .v {
    word-break: break-all;
  }
  .digest {
    color: var(--text);
  }
  .copy,
  .prov-links a {
    font-size: 0.72rem;
  }
  .copy {
    padding: 0.05rem 0.4rem;
    border: 1px solid var(--border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text-muted);
    cursor: pointer;
  }
  .prov-links {
    display: flex;
    gap: 0.8rem;
    margin: 0.2rem 0 0.4rem;
  }
  .mono {
    font-size: 0.78rem;
  }
  .images {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.8rem;
  }
  .images th {
    text-align: left;
    color: var(--text-muted);
    font-weight: normal;
    border-bottom: 1px solid var(--border);
    padding: 0.2rem 0.4rem;
  }
  .images td {
    padding: 0.25rem 0.4rem;
    vertical-align: baseline;
  }
  .images .ref {
    word-break: break-all;
  }
  .images .reason td {
    color: var(--fg-fail);
    font-size: 0.75rem;
  }
  .state {
    font-size: 0.72rem;
    padding: 0 0.35rem;
    border-radius: 0.4rem;
    border: 1px solid var(--border);
  }
  .state.ready {
    color: var(--fg-ok);
  }
  .state.building,
  .state.unbuilt {
    color: var(--fg-warn);
  }
  .state.failed {
    color: var(--fg-fail);
  }
  .derived {
    font-size: 0.75rem;
    margin-top: 0.15rem;
  }
  .archived {
    margin-left: 0.3rem;
    color: var(--text-muted);
  }
  .muted {
    font-size: 0.85rem;
  }
  .kind {
    font-family: inherit;
    font-size: 0.68rem;
    padding: 0.05rem 0.35rem;
    margin-right: 0.3rem;
    border: 1px solid var(--border);
    border-radius: 0.3rem;
    color: var(--text-muted);
  }
  .arch-builds {
    display: flex;
    gap: 0.5rem;
    margin-top: 0.2rem;
    font-size: 0.75rem;
  }
  .arch.failed {
    color: var(--fg-fail);
  }
  .generated pre {
    margin: 0.3rem 0 0;
    padding: 0.5rem;
    font-size: 0.72rem;
    overflow-x: auto;
    border: 1px solid var(--border);
    border-radius: 0.4rem;
  }
</style>
