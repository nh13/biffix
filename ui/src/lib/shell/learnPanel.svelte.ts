// The Learn panel's open state ("?" beside the bell): open across pages until closed, showing the
// current page's help. Closing it hands focus back to the "?" that opened it.

import { tick } from 'svelte';

export class LearnPanelState {
  open = $state(false);
  #opener: HTMLElement | null = null;

  /** The "?" was pressed: open the panel from it, or close it when open. */
  toggle(e: MouseEvent): void {
    if (this.open) {
      void this.close();
      return;
    }
    this.#opener = e.currentTarget as HTMLElement;
    this.open = true;
  }

  async close(): Promise<void> {
    this.open = false;
    // The page is no longer inert once this renders; an inert button cannot take focus.
    await tick();
    // The opener, else whichever "?" is on show (the other one is hidden at this width).
    const opener = this.#opener;
    const back =
      opener?.checkVisibility?.() !== false
        ? opener
        : [...document.querySelectorAll<HTMLElement>('.learn-btn')].find((b) =>
            b.checkVisibility?.(),
          );
    back?.focus();
  }
}
