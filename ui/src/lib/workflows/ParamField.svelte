<script lang="ts">
  // One parameter field, read-only: its name, widget, badges, description and choices.
  import { humanizeWidget, isRequiredInput, type FormField } from './schema';

  interface Props {
    field: FormField;
  }
  let { field }: Props = $props();
</script>

<div class="field">
  <div class="head">
    <code class="name">{field.name}</code>
    <span class="widget">{humanizeWidget(field)}</span>
    {#if isRequiredInput(field)}<span class="badge req">required</span>{/if}
    {#if field.is_secret}<span class="badge secret" title="Secret — do not commit">🔒 secret</span
      >{/if}
  </div>
  {#if field.description}<p class="desc">{field.description}</p>{/if}
  {#if field.enum_values && field.enum_values.length}
    <div class="choices">
      {#each field.enum_values as choice}<span class="choice">{choice}</span>{/each}
    </div>
  {/if}
</div>

<style>
  .field {
    padding: 0.5rem 0;
    border-bottom: 1px solid var(--border);
  }
  .field:last-child {
    border-bottom: none;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
  }
  .name {
    font-family: var(--mono);
    font-size: 0.82rem;
    color: var(--text);
  }
  .widget {
    font-size: 0.72rem;
    color: var(--text-muted);
  }
  .badge {
    font-size: 0.66rem;
    padding: 0.02rem 0.35rem;
    border-radius: 0.6rem;
    border: 1px solid var(--border);
    line-height: 1.4;
  }
  .badge.req {
    color: var(--fg-fail);
    border-color: #e5484d55;
  }
  .badge.secret {
    color: var(--text-muted);
  }
  .desc {
    margin: 0.25rem 0 0;
    font-size: 0.78rem;
    color: var(--text-muted);
  }
  .choices {
    display: flex;
    gap: 0.3rem;
    flex-wrap: wrap;
    margin-top: 0.3rem;
  }
  .choice {
    font-size: 0.68rem;
    padding: 0.02rem 0.4rem;
    border-radius: 0.6rem;
    border: 1px solid var(--border);
    color: var(--text-muted);
  }
</style>
