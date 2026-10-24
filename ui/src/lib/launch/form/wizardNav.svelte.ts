// The wizard's (Lab view's) step navigation: the same form, a step at a time: Add samples (2),
// Reference & options (3), Review & launch (4). Step 1, picking the pipeline, is New run. `?step=`
// names the step, so Back, a reload and a shared link all land on it. A step change moves focus to
// the new step: to the field it was made for (the form's `focus`), else the step's heading.
//
// Construct it while a component initialises: its effects and its `afterNavigate` hook live as long
// as that component.

import { tick, untrack } from 'svelte';
import { afterNavigate, goto } from '$app/navigation';
import { page } from '$app/state';
import { withExperiment } from '#lib/experiments/experiments.js';
import { replaceUrl } from '#lib/util/replaceUrl.js';
import type { Problem } from '../form';
import {
  canReach,
  firstBlockedStep,
  parseStep,
  plainClick,
  stepProblems,
  type FormStep,
  type Placement,
  type WizardStep,
} from '../wizard';
import { focusStepHeading, type FocusTarget } from './fieldFocus';

/** What the steps need of the form: read through getters, so they stay current. */
export interface WizardInputs {
  /** Whether the form is shown as the wizard (else nothing here moves). */
  readonly wizard: boolean;
  /** What blocks the launch, in the form's field order. */
  readonly problems: Problem[];
  /** The step each field is on. */
  readonly placed: Placement;
  /** The experiment being launched into, which Back to step 1 keeps. */
  readonly experiment: string | null;
  /** Whether the form has loaded (a Re-run's pre-fill, the reference choices), so a step reached
   * by its URL can be judged. */
  readonly loaded: boolean;
  /** Focus a field or a problem on the step now shown. */
  focus(target: FocusTarget): void;
}

export class WizardNav {
  readonly #in: WizardInputs;
  readonly step = $derived(parseStep(page.url.searchParams.get('step')));
  readonly formStep = $derived<FormStep>(this.step === 1 ? 2 : this.step);
  // Back to step 1 keeps the experiment being launched into.
  readonly newRunHref = $derived.by(() => withExperiment('/runs/new', this.#in.experiment));
  /** What blocks leaving the current step (on Review, anything that blocks the launch). */
  readonly blocking = $derived.by(() =>
    stepProblems(this.#in.problems, this.#in.placed, this.formStep),
  );
  // A step reached by its URL (a link, a reload) that an earlier step blocks goes back to that
  // step, judged once the form has loaded. Only on arrival: a change made on this step never moves
  // it.
  #arrived = $state(true);
  /** The field to focus once the step it is on has rendered (else the step's heading). */
  #pendingFocus: FocusTarget | null = null;
  // A step change (Next, Back, the stepper, the browser's Back) moves focus to the new step.
  #shownStep: WizardStep;

  constructor(inputs: WizardInputs) {
    this.#in = inputs;
    this.#shownStep = untrack(() => this.step);

    $effect(() => {
      if (inputs.wizard && this.step === 1) goto(this.newRunHref, { replace: true });
    });
    $effect(() => {
      if (!inputs.wizard || !this.#arrived || !inputs.loaded) return;
      this.#arrived = false;
      if (this.step === 1 || canReach(this.step, inputs.problems, inputs.placed)) return;
      const blocked = firstBlockedStep(inputs.problems, inputs.placed);
      if (blocked) replaceUrl(this.stepHref(blocked));
    });
    afterNavigate(() => {
      this.#arrived = true;
      const s = this.step;
      if (inputs.wizard && s !== this.#shownStep) void this.#focusStep();
      this.#shownStep = s;
    });
  }

  stepHref = (n: WizardStep): string => {
    if (n === 1) return this.newRunHref;
    const u = new URL(page.url.href);
    u.searchParams.set('step', String(n));
    return `${u.pathname}${u.search}`;
  };
  /** A step is a link when every step before it is complete. */
  stepLink = (n: WizardStep): string | null =>
    n === 1 || canReach(n, this.#in.problems, this.#in.placed) ? this.stepHref(n) : null;
  stepDone = (n: WizardStep): boolean =>
    n < this.formStep &&
    (n === 1 || stepProblems(this.#in.problems, this.#in.placed, n as FormStep).length === 0);
  /** Go to step `n`, focusing `focus` once it has rendered. */
  goStep = (n: WizardStep, focus?: FocusTarget) => {
    if (n === 1) {
      goto(this.newRunHref);
      return;
    }
    this.#pendingFocus = focus ?? null;
    goto(this.stepHref(n), { reset: false });
  };
  /** A step link's click: a plain one moves in place, a new-tab one opens the link. */
  navTo = (e: MouseEvent, n: WizardStep) => {
    if (!plainClick(e)) return;
    e.preventDefault();
    this.goStep(n);
  };

  async #focusStep() {
    await tick();
    const target = this.#pendingFocus;
    this.#pendingFocus = null;
    if (target !== null) this.#in.focus(target);
    else focusStepHeading();
  }
}
