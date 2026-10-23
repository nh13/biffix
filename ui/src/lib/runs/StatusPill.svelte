<script lang="ts">
  // A run's status, as each view shows it. Lab: a coloured plain-language label plus what is known
  // beyond it (`statusLine`). Expert: the state's own name in a badge (the `.badge.<state>` rules in
  // app.css).
  import { runStateLabel } from './runState';
  import type { StatusLine } from './status';

  type Props =
    | { line: StatusLine; compact?: boolean }
    | {
        /** The run's state, shown as a badge. */
        state: string;
        /** Announce a change (the one status of a run's page). */
        polite?: boolean;
      };
  let props: Props = $props();
</script>

{#if 'line' in props}
  {@const line = props.line}
  <span class="status {line.tone}" class:compact={props.compact ?? false}>
    <span class="label">{line.label}</span>{#if line.detail}<span class="detail"
        >{' · '}{line.detail}</span
      >{/if}
  </span>
{:else}
  <span class={`badge ${props.state}`} aria-live={props.polite ? 'polite' : undefined}
    >{runStateLabel(props.state)}</span
  >
{/if}

<style>
  .status {
    font-size: 0.82rem;
  }
  .label {
    font-weight: 600;
  }
  .detail {
    color: var(--text-muted);
  }
  .waiting .label,
  .stopped .label {
    color: var(--text-muted);
  }
  .running .label {
    color: var(--accent);
  }
  .done .label {
    color: var(--fg-ok);
  }
  .attention .label {
    color: var(--fg-fail);
  }
  .compact {
    font-size: 0.78rem;
  }
</style>
