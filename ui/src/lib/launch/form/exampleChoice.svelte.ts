// The launch form's examples: the *Start from example* menu, the replace it asks about first, the
// *Undo* that puts back what an example replaced, and the first-time banner that points at them.
// The form owns what is entered; this asks it for a snapshot, to fill in an example, and to restore
// a snapshot (`ExampleForm`), so it never reaches into the form's fields.
//
// Construct it while a component initialises: its effect lives as long as that component.

import { untrack } from 'svelte';
import { readPref, writePref } from '#lib/util/storage.js';
import { fingerprint, type FormSnapshot } from './formEntry';

const BANNER_KEY = (name: string) => `biffix:launch-banner-dismissed:${name}`;

/** What the examples need of the form: read through getters, so they stay current. */
export interface ExampleForm {
  /** The workflow's name (a dismissed banner stays dismissed for it). */
  readonly name: string;
  /** Whether an example would overwrite something: what the user entered, or a Re-run's pre-fill. */
  readonly hasInput: boolean;
  /** The form as it is now. */
  snapshot(): FormSnapshot;
  /** Replace the whole form with the `i`th example (a reset first, which calls `reset()` here). */
  fill(i: number): void;
  /** Put the form back as it was. */
  restore(s: FormSnapshot): void;
}

export class ExampleChoice {
  readonly #form: ExampleForm;
  /** Whether the examples menu is open. */
  open = $state(false);
  /** The example the menu is asking about replacing the form with. */
  pending = $state<number | null>(null);
  /** The form as it was before an example replaced it, for *Undo*. */
  before = $state<FormSnapshot | null>(null);
  /** Whether the first-time banner was dismissed. */
  bannerDismissed = $state(false);
  /** The form just after the example went in: an edit since then is lost by *Undo*, so it asks. */
  #applied: string | null = null;

  constructor(form: ExampleForm) {
    this.#form = form;
    $effect(() => {
      const name = form.name;
      untrack(() => {
        this.bannerDismissed = readPref(BANNER_KEY(name)) === '1';
      });
    });
  }

  /** Forget the replace being asked about and the *Undo* (a fresh form). */
  reset() {
    this.pending = null;
    this.before = null;
  }

  /** Open or close the menu, dropping any replace it was asking about. */
  setOpen(open: boolean) {
    this.open = open;
    this.pending = null;
  }

  dismissBanner() {
    this.bannerDismissed = true;
    writePref(BANNER_KEY(this.#form.name), '1');
  }

  /** Fill in the `i`th example; over what was entered, only once asked twice (the confirm). */
  choose(i: number) {
    const form = this.#form;
    if (form.hasInput && this.pending !== i) {
      this.pending = i;
      return;
    }
    // Kept so *Undo* can put back what the example replaced.
    const before = form.hasInput ? form.snapshot() : null;
    form.fill(i);
    this.before = before;
    this.open = false;
    this.#applied = before ? fingerprint(form.snapshot()) : null;
  }

  /** Put back what the example replaced, asking first when it was edited since. */
  undo() {
    const b = this.before;
    if (!b) return;
    if (
      this.#applied !== null &&
      fingerprint(this.#form.snapshot()) !== this.#applied &&
      !confirm('Undo the example? The changes you made since will be lost too.')
    )
      return;
    this.#applied = null;
    this.#form.restore(b);
    this.before = null;
  }
}
