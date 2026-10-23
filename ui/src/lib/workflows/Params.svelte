<script lang="ts">
  // The Parameters tab: a pre-launch checklist. Summary line + registration warnings +
  // three groups (required inputs / optional inputs / outputs) + a copyable launch scaffold.
  import { parseSchema, groupFields, summarize, scaffoldYaml } from './schema';
  import ParamField from './ParamField.svelte';
  import { paramsCommand } from '$lib/launch/form';
  import { CopiedFlag } from '$lib/util/clipboard.svelte';

  interface Props {
    /** Opaque `form_schema` JSON from the workflow detail. */
    formSchema: unknown;
    /** The frozen version id, for the `biffix runs submit` command line. */
    workflowId: string;
  }
  let { formSchema, workflowId }: Props = $props();

  const schema = $derived(parseSchema(formSchema));
  const sections = $derived.by(() => {
    const g = groupFields(schema);
    return [
      ['Required inputs', g.requiredInputs],
      ['Optional inputs', g.optionalInputs],
      ['Outputs', g.outputs],
    ] as const;
  });
  const summaryLine = $derived(summarize(schema));
  const empty = $derived(schema.fields.length === 0);

  const scaffold = $derived(`# ${paramsCommand(workflowId)}\n` + scaffoldYaml(schema));
  const copied = new CopiedFlag();
</script>

<div class="params">
  <p class="summary">{summaryLine}</p>

  {#if schema.warnings.length}
    <div class="warnings">
      <strong>Registration warnings</strong>
      <ul>
        {#each schema.warnings as w}<li>{w}</li>{/each}
      </ul>
    </div>
  {/if}

  {#if empty}
    <p class="muted">This workflow version declares no parameters.</p>
  {:else}
    {#each sections as [heading, fields] (heading)}
      {#if fields.length}
        <section>
          <h3>{heading}</h3>
          {#each fields as f}<ParamField field={f} />{/each}
        </section>
      {/if}
    {/each}

    <section class="scaffold">
      <div class="scaffold-head">
        <h3>Launch scaffold</h3>
        <button onclick={() => copied.copy(scaffold, 'scaffold')}
          >{copied.is('scaffold') ? 'Copied ✓' : 'Copy'}</button
        >
      </div>
      <!-- Focusable, so its long lines scroll from the keyboard too. -->
      <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
      <pre tabindex="0" aria-label="params.yaml scaffold">{scaffold}</pre>
    </section>
  {/if}
</div>

<style>
  .summary {
    font-size: 0.9rem;
    color: var(--text);
    margin: 0 0 0.75rem;
  }
  .warnings {
    border: 1px solid #e5a04d55;
    background: #e5a04d11;
    border-radius: 0.4rem;
    padding: 0.5rem 0.75rem;
    margin-bottom: 0.75rem;
    font-size: 0.8rem;
  }
  .warnings ul {
    margin: 0.3rem 0 0;
    padding-left: 1.1rem;
  }
  section {
    margin-bottom: 1.25rem;
  }
  h3 {
    font-size: 0.78rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-muted);
    margin: 0 0 0.25rem;
  }
  .scaffold-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .scaffold-head button {
    padding: 0.2rem 0.6rem;
    border: 1px solid var(--border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text);
    cursor: pointer;
    font-size: 0.75rem;
  }
  pre {
    margin: 0.25rem 0 0;
    padding: 0.6rem 0.75rem;
    background: var(--bg-content);
    border: 1px solid var(--border);
    border-radius: 0.4rem;
    overflow-x: auto;
    font-family: var(--mono);
    font-size: 0.74rem;
    line-height: 1.5;
    white-space: pre;
  }
  .muted {
    font-size: 0.85rem;
  }
</style>
