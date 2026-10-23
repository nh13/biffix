// The reference bundles a launch form's dropdown can offer: those of its kind with every key it
// requires (`null` while loading). When none fits, whether any is registered at all is asked too
// (an admin's first job), and a form whose author allows the params' own fields opens on them.
//
// Construct it while a component initialises: its effect lives as long as that component.

import { untrack } from 'svelte';
import { listReferences, type ReferenceBundle } from '$lib/api';
import { errorMessage } from '$lib/util/format';
import type { FormReferences } from '$lib/workflows/schema';

export class ReferenceOffers {
  offered = $state<ReferenceBundle[] | null>(null);
  error = $state<string | null>(null);
  /** Whether any reference bundle is registered at all, asked only when none fits this form. */
  anyRegistered = $state<boolean | null>(null);
  readonly #refs: () => FormReferences | null;
  readonly #noneFits: () => void;

  /**
   * `refs`: the form's reference dropdown, if it has one. `noneFits` is called (untracked) when no
   * bundle fits a dropdown that allows the params' own fields.
   */
  constructor(refs: () => FormReferences | null, noneFits: () => void) {
    this.#refs = refs;
    this.#noneFits = noneFits;
    $effect(() => {
      const r = refs();
      this.offered = null;
      this.error = null;
      if (!r) return;
      let cancelled = false;
      listReferences({ kind: r.kind, keys: r.required })
        .then((b) => {
          if (cancelled) return;
          this.offered = b;
          // None fits: is it that none is registered at all (an admin's first job)?
          this.anyRegistered = null;
          if (b.length === 0)
            listReferences()
              .then((all) => {
                if (!cancelled) this.anyRegistered = all.length > 0;
              })
              .catch(() => {});
          // Nothing to pick from: open on the params' own fields, when the author allows it.
          if (b.length === 0 && r.custom) untrack(noneFits);
        })
        .catch((e) => {
          if (!cancelled) {
            this.offered = [];
            this.error = errorMessage(e);
          }
        });
      return () => {
        cancelled = true;
      };
    });
  }

  /**
   * Applies the none-fits rule again to a form the user reset (an example, a discarded draft): the
   * load ran it once, and nothing re-runs the load for a reset.
   */
  noneFitsAgain() {
    if (this.#refs()?.custom && this.offered?.length === 0 && !this.error) this.#noneFits();
  }
}
