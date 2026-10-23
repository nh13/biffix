// A disclosure's behaviour, shared by the popovers in the app shell and on pages (the notification
// bell, Share): a button toggles a panel after it; Escape closes it and hands focus back to the
// button; focus or a click landing outside closes it, leaving focus where it went.
//
//   const d = new Disclosure();
//   <svelte:window onpointerdown={d.onWindowPointer} />
//   <div bind:this={d.root} onkeydown={d.onkeydown} onfocusout={d.onfocusout}>
//     <button bind:this={d.button} aria-expanded={d.open} onclick={() => d.toggle()}>…</button>
//     {#if d.open}<div>…</div>{/if}
//   </div>

export class Disclosure {
  /** Whether the panel is shown. */
  open = $state(false);
  /** The element holding the button and its panel: focus or a click outside it closes. */
  root = $state<HTMLElement | null>(null);
  /** The button that toggles the panel, where Escape returns focus. */
  button = $state<HTMLElement | null>(null);

  toggle(): void {
    this.open = !this.open;
  }

  /** Close the panel; with `focus`, hand focus back to the button. */
  close({ focus = false } = {}): void {
    this.open = false;
    if (focus) this.button?.focus();
  }

  /** Escape closes an open panel (and is not passed on to a dialog or page behind it). */
  onkeydown = (e: KeyboardEvent): void => {
    if (e.key === 'Escape' && this.open) {
      e.stopPropagation();
      this.close({ focus: true });
    }
  };

  /** Focus moving outside closes it. */
  onfocusout = (e: FocusEvent): void => {
    const to = e.relatedTarget as Node | null;
    if (this.open && to && !this.root?.contains(to)) this.close();
  };

  /** A press outside closes it (bind to the window's `pointerdown`). */
  onWindowPointer = (e: PointerEvent): void => {
    if (this.open && !this.root?.contains(e.target as Node)) this.close();
  };
}
