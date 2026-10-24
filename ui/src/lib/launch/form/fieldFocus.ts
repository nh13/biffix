// Going to a field or a problem in the launch form: which wizard step it is fixed on, and moving
// focus to its element (a samplesheet row's cell, a field in a collapsed group or disclosure). The
// form decides when to go (`LaunchForm.focusField`, `focusProblem`); this finds where.

import type { SheetSpec } from '#lib/generated/samples.js';
import type { Problem } from '../form';
import { problemColumn, sheetCellId } from '../samplesheet/sheet';
import { stepOfField, type Placement, type WizardStep } from '../wizard';

/** A field to focus, by name, or a problem to go to. */
export type FocusTarget = string | Problem;

/** In the wizard, the step a field on another step is fixed on; `null`: it is on this one. */
export function stepToFix(
  field: string,
  at: { wizard: boolean; step: WizardStep; placed: Placement },
): WizardStep | null {
  if (!at.wizard) return null;
  const target = stepOfField(field, at.placed);
  return target === at.step ? null : target;
}

/** Scroll an element into view and focus it. */
function focusOn(el: HTMLElement) {
  el.scrollIntoView({ block: 'center', behavior: 'smooth' });
  el.focus({ preventScroll: true });
}

/**
 * Focus the cell a samplesheet row's problem is about (its column's, else the row's first).
 * Returns whether there was one to focus.
 */
export function focusCell(p: Problem, spec: SheetSpec | null | undefined): boolean {
  if (p.row === undefined) return false;
  const column = spec ? problemColumn(spec, p.column) : null;
  const el = column ? document.getElementById(sheetCellId(p.field, p.row, column)) : null;
  if (!el) return false;
  focusOn(el);
  return true;
}

/**
 * Focus a field's control. One in a collapsed Advanced group is not rendered yet: its group is
 * opened (`openAdvanced`, and the group's own `<details>`) and it is focused once it has rendered.
 */
export function focusFieldControl(name: string, openAdvanced: () => void): void {
  const el = document.getElementById(`field-${name}`);
  if (!el) {
    const details = document.querySelector<HTMLDetailsElement>(`[data-holds~="${name}"]`);
    if (details) details.open = true;
    openAdvanced();
    requestAnimationFrame(() => document.getElementById(`field-${name}`)?.focus());
    return;
  }
  // A field inside a collapsed disclosure (the Review's *Where results go*) is shown first.
  for (let d = el.closest('details'); d; d = d.parentElement?.closest('details') ?? null)
    if (!d.open) d.open = true;
  focusOn(el);
}

/** Focus the wizard's step heading (a step change with no field to go to). */
export function focusStepHeading(): void {
  document.getElementById('wizard-heading')?.focus();
}
