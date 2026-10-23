<script lang="ts">
  // One home for the app's inline stroked icons (lucide-style) — no icon dependency. Callers write
  // <Icon name="runs" />; the shared wrapper attributes (viewBox, stroke, linecap/linejoin) live
  // here once. `class`/`style` pass through so a
  // caller can still colour an icon (the status/activity badge tints by health).
  interface Props {
    name:
      | 'home'
      | 'runs'
      | 'workflows'
      | 'data'
      | 'usage'
      | 'api'
      | 'settings'
      | 'activity'
      | 'sun'
      | 'moon'
      | 'logout'
      | 'panel-close'
      | 'panel-open'
      | 'menu'
      | 'bell'
      | 'experiments';
    class?: string;
    style?: string;
  }
  let { name, class: klass = '', style = '' }: Props = $props();
</script>

<!-- Decorative: the control around an icon carries its label, so screen readers skip the glyph. -->
<svg
  aria-hidden="true"
  focusable="false"
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="2"
  stroke-linecap="round"
  stroke-linejoin="round"
  class={klass}
  {style}
>
  {#if name === 'experiments'}
    <path d="M9 3h6" /><path
      d="M10 3v6.5L4.5 19a1.5 1.5 0 0 0 1.3 2.2h12.4a1.5 1.5 0 0 0 1.3-2.2L14 9.5V3"
    /><path d="M7.5 15h9" />
  {:else if name === 'home'}
    <path d="m3 9 9-7 9 7v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" /><path d="M9 22V12h6v10" />
  {:else if name === 'runs'}
    <line x1="8" y1="6" x2="21" y2="6" /><line x1="8" y1="12" x2="21" y2="12" /><line
      x1="8"
      y1="18"
      x2="21"
      y2="18"
    /><line x1="3" y1="6" x2="3.01" y2="6" /><line x1="3" y1="12" x2="3.01" y2="12" /><line
      x1="3"
      y1="18"
      x2="3.01"
      y2="18"
    />
  {:else if name === 'workflows'}
    <rect x="3" y="3" width="7" height="7" rx="1" /><rect
      x="14"
      y="3"
      width="7"
      height="7"
      rx="1"
    />
    <rect x="3" y="14" width="7" height="7" rx="1" /><path d="M14 17.5h7" /><path d="M17.5 14v7" />
  {:else if name === 'usage'}
    <path d="M3 3v18h18" /><path d="M18 17V9" /><path d="M13 17V5" /><path d="M8 17v-3" />
  {:else if name === 'data'}
    <ellipse cx="12" cy="5" rx="9" ry="3" /><path d="M3 5v14a9 3 0 0 0 18 0V5" /><path
      d="M3 12a9 3 0 0 0 18 0"
    />
  {:else if name === 'api'}
    <polyline points="16 18 22 12 16 6" /><polyline points="8 6 2 12 8 18" />
  {:else if name === 'settings'}
    <circle cx="12" cy="12" r="3" /><path
      d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"
    />
  {:else if name === 'activity'}
    <path d="M22 12h-4l-3 9L9 3l-3 9H2" />
  {:else if name === 'sun'}
    <circle cx="12" cy="12" r="4" /><path
      d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M6.3 17.7l-1.4 1.4M19.1 4.9l-1.4 1.4"
    />
  {:else if name === 'moon'}
    <path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z" />
  {:else if name === 'logout'}
    <path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4" /><polyline points="16 17 21 12 16 7" /><line
      x1="21"
      y1="12"
      x2="9"
      y2="12"
    />
  {:else if name === 'panel-close'}
    <rect x="3" y="3" width="18" height="18" rx="2" /><path d="M9 3v18" /><path
      d="m16 15-3-3 3-3"
    />
  {:else if name === 'panel-open'}
    <rect x="3" y="3" width="18" height="18" rx="2" /><path d="M9 3v18" /><path d="m14 9 3 3-3 3" />
  {:else if name === 'bell'}
    <path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9" /><path
      d="M10.3 21a1.94 1.94 0 0 0 3.4 0"
    />
  {:else if name === 'menu'}
    <line x1="4" y1="6" x2="20" y2="6" /><line x1="4" y1="12" x2="20" y2="12" /><line
      x1="4"
      y1="18"
      x2="20"
      y2="18"
    />
  {/if}
</svg>
