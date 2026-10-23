<script lang="ts">
  // The one modal dialog: a native `<dialog>` opened with `showModal()` while it is mounted, so the
  // page behind is inert and the dialog sits in the top layer. Focus moves in on open and stays in
  // (Tab wraps through the dialog's own controls), and returns to whatever opened it on close.
  // Escape or the caller's own Close button closes it, and so does the backdrop unless the caller
  // says otherwise. An Escape something inside already handled (`preventDefault`: a field of the
  // genome browser's own) does not. The caller unmounts it to close it (`onclose` asks it to).
  import { untrack, type Snippet } from 'svelte';

  interface Props {
    /** The id of the element that names the dialog (its heading). */
    labelledby: string;
    onclose: () => void;
    children: Snippet;
    /**
     * Its shape: a file `viewer` (the default), a `wide` one (a genome browser), a `picker` (the
     * file picker's fixed-size panel), or a short `confirm` question.
     */
    size?: 'viewer' | 'wide' | 'picker' | 'confirm';
    /**
     * Where focus returns on close, when not whatever had it on open: a viewer that swaps one dialog
     * for another (FastQC → its full report → back) passes the button that opened the first.
     */
    returnFocus?: HTMLElement | null;
    /** Whether a click on the backdrop closes it (a form's dialog keeps what was typed). */
    closeOnBackdrop?: boolean;
  }
  let {
    labelledby,
    onclose,
    children,
    size = 'viewer',
    returnFocus,
    closeOnBackdrop = true,
  }: Props = $props();

  // A viewer takes focus itself (its content is read, not filled in); a form's dialog lets the
  // browser focus its first control.
  const form = $derived(size === 'picker' || size === 'confirm');

  let dialog = $state<HTMLDialogElement | null>(null);
  $effect(() => {
    if (!dialog) return;
    const given = untrack(() => returnFocus);
    const opener =
      given !== undefined
        ? given
        : document.activeElement instanceof HTMLElement
          ? document.activeElement
          : null;
    dialog.showModal();
    if (!untrack(() => form)) dialog.focus();
    return () => {
      if (opener?.isConnected) opener.focus();
    };
  });

  const FOCUSABLE =
    'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), ' +
    'textarea:not([disabled]), iframe, [tabindex]:not([tabindex="-1"])';

  /** Keep Tab / Shift+Tab cycling through the dialog's own controls. */
  function trapTab(e: KeyboardEvent) {
    if (!dialog) return;
    const focusable = [...dialog.querySelectorAll<HTMLElement>(FOCUSABLE)].filter(
      (el) => el.getClientRects().length > 0,
    );
    const active = document.activeElement;
    if (focusable.length === 0) {
      e.preventDefault();
      dialog.focus();
      return;
    }
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    const inside = active instanceof Node && dialog.contains(active);
    if (e.shiftKey && (!inside || active === first || active === dialog)) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && (!inside || active === last)) {
      e.preventDefault();
      first.focus();
    }
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      if (!e.defaultPrevented) onclose();
    } else if (e.key === 'Tab') trapTab(e);
  }

  /** Whether a press or click landed on the backdrop: the dialog itself, outside its box. */
  function onBackdrop(e: MouseEvent): boolean {
    if (!dialog || e.target !== dialog) return false;
    const r = dialog.getBoundingClientRect();
    return !(
      e.clientX >= r.left &&
      e.clientX <= r.right &&
      e.clientY >= r.top &&
      e.clientY <= r.bottom
    );
  }
  // A drag that starts inside (selecting a viewer's text) and ends past the edge clicks the dialog
  // outside its box too: only a press that also started on the backdrop closes it.
  let pressedBackdrop = false;
  function onpointerdown(e: PointerEvent) {
    pressedBackdrop = onBackdrop(e);
  }
  function onclick(e: MouseEvent) {
    const close = closeOnBackdrop && pressedBackdrop && onBackdrop(e);
    pressedBackdrop = false;
    if (close) onclose();
  }
</script>

<svelte:window {onkeydown} />

<!-- The backdrop is a plain click target; keyboard users close with Escape or the Close button. -->
<dialog
  class="dialog {size}"
  aria-labelledby={labelledby}
  tabindex="-1"
  bind:this={dialog}
  {onpointerdown}
  {onclick}
  oncancel={(e) => {
    // Escape is handled above (it may already be someone else's); the browser does not close it.
    e.preventDefault();
  }}
  onclose={() => {
    // Closed by the browser itself (an Escape it would not let be cancelled): say so.
    if (dialog?.isConnected) onclose();
  }}
>
  {@render children()}
</dialog>

<style>
  .dialog {
    display: flex;
    flex-direction: column;
    padding: 0;
    color: var(--text);
    background: var(--bg-content);
    outline: none;
  }
  .viewer,
  .wide {
    width: min(72rem, calc(100vw - 2rem));
    max-width: none;
    max-height: calc(100vh - 2rem);
    overflow: visible;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: 0 10px 40px rgb(0 0 0 / 0.3);
  }
  .wide {
    width: min(90rem, calc(100vw - 2rem));
  }
  .viewer::backdrop,
  .wide::backdrop {
    background: rgb(0 0 0 / 0.45);
  }
  .picker,
  .confirm {
    border: 1px solid var(--control-border);
    border-radius: var(--radius-xl);
  }
  .picker::backdrop,
  .confirm::backdrop {
    background: rgb(0 0 0 / 0.35);
  }
  .picker {
    width: min(58rem, 96vw);
    height: min(40rem, 90vh);
  }
  .confirm {
    display: block;
    width: min(28rem, calc(100vw - 2rem));
    padding: 1rem 1.2rem;
  }
  @media (max-width: 800px) {
    .picker {
      width: 100vw;
      height: 100vh;
      max-width: 100vw;
      max-height: 100vh;
      border-radius: 0;
    }
  }
</style>
