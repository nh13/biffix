// The launch form's reference dropdown: a version's form may fill several
// params (fasta, gtf, an index…) from one admin-registered reference bundle. The user picks a
// bundle or chooses *Use my own files…*, which shows those params' own fields instead. Pure, so it
// is unit-tested apart from rendering.

import type { ReferenceBundle } from '#lib/api/index.js';
import type { FormReferences, FormSchema } from '#lib/workflows/schema.js';
import type { FormValues } from './form';
import type { ValidationError } from './validate';

/** What the user chose: a bundle id (or none yet), or their own files. */
export interface RefChoice {
  bundle: string | null;
  custom: boolean;
}

export const NO_CHOICE: RefChoice = { bundle: null, custom: false };

/** The problem key the reference dropdown's own problem is reported under (for the footer). */
export const REFERENCE_FIELD = '__reference';

/**
 * Stands in, for validation, for the value of a param a chosen bundle fills while that bundle is
 * not loaded yet. `referenceErrors` drops whatever validation says about it, so only `required`
 * sees it.
 */
const FILLED_BY_BUNDLE = 's3://reference-bundle/filled-by-biffix';

/** Whether the form takes its reference params from a bundle (rather than the user's own files). */
export function usesBundle(schema: FormSchema, choice: RefChoice): boolean {
  return !!schema.references && !(choice.custom && schema.references.custom);
}

/** The bundle the choice names, once `offered` holds it (`null` while loading, or none offered). */
export function chosenBundle(
  choice: RefChoice,
  offered: ReferenceBundle[] | null,
): ReferenceBundle | null {
  return (choice.bundle && offered?.find((b) => b.id === choice.bundle)) || null;
}

/**
 * The params a bundle fills; they are hidden while a bundle provides them. With the chosen `bundle`
 * in hand, only the params whose key it holds: the server leaves a param whose key the bundle lacks
 * as given, so that param stays on the form. Without it (none chosen yet, or still loading), every
 * param the form's bundles may fill.
 */
export function bundleParams(
  schema: FormSchema,
  choice: RefChoice,
  bundle?: ReferenceBundle | null,
): Set<string> {
  if (!usesBundle(schema, choice)) return new Set<string>();
  const fill = Object.entries(schema.references!.fill);
  return new Set(
    fill.filter(([, key]) => !bundle || Object.hasOwn(bundle.uris, key)).map(([param]) => param),
  );
}

/** What a bundle fills for this form (`key → uri`, in fill order) and the optional keys it
 * lacks. */
export function fillsOf(
  bundle: ReferenceBundle,
  refs: FormReferences,
): { fills: { key: string; uri: string }[]; lacks: string[] } {
  const keys = [...new Set(Object.values(refs.fill))];
  return {
    fills: keys
      .filter((k) => Object.hasOwn(bundle.uris, k))
      .map((k) => ({ key: k, uri: bundle.uris[k] })),
    lacks: keys.filter((k) => !Object.hasOwn(bundle.uris, k)),
  };
}

/**
 * The values as validation sees them (a new object): a filled param holds the chosen bundle's URI,
 * as the server fills it before validating. Until the bundle is chosen and loaded its params still
 * count as given (the dropdown reports the one problem, not each param it would fill); run the
 * errors through `referenceErrors`, which drops what validation says about those stand-ins.
 */
export function valuesForValidation(
  schema: FormSchema,
  values: FormValues,
  choice: RefChoice,
  bundle?: ReferenceBundle | null,
): FormValues {
  const fill = schema.references?.fill ?? {};
  const filled = [...bundleParams(schema, choice, bundle)].map((p) => [
    p,
    bundle ? bundle.uris[fill[p]] : FILLED_BY_BUNDLE,
  ]);
  return { ...values, ...Object.fromEntries(filled) };
}

/**
 * The validation errors as the form reports them, given values from `valuesForValidation`. An error
 * on a param the bundle fills can't be fixed in its (hidden) field: with the bundle loaded it is
 * the server's own verdict on the bundle's URI, so it is reported on the dropdown (choose another);
 * while the bundle is unknown it is about the stand-in, so it is dropped.
 */
export function referenceErrors(
  schema: FormSchema,
  choice: RefChoice,
  bundle: ReferenceBundle | null,
  errors: ValidationError[],
): ValidationError[] {
  const filled = bundleParams(schema, choice, bundle);
  return errors.flatMap((e) => {
    if (!filled.has(e.field)) return [e];
    if (!bundle) return [];
    return [{ field: REFERENCE_FIELD, message: `${e.field} ${e.message}` }];
  });
}

/**
 * The values to submit (a new object): without the params the bundle fills (the server fills them
 * from it). A param whose key the chosen bundle lacks is submitted as given.
 */
export function valuesForSubmit(
  schema: FormSchema,
  values: FormValues,
  choice: RefChoice,
  bundle?: ReferenceBundle | null,
): FormValues {
  const filled = bundleParams(schema, choice, bundle);
  return Object.fromEntries(Object.entries(values).filter(([k]) => !filled.has(k)));
}

/**
 * What is wrong with the choice, if anything: a form that needs a bundle has none chosen, or the
 * chosen one is no longer offered. `offered` is `null` while bundles are still loading; `loadError`
 * says they failed to, which is neither (there is nothing to judge the choice against).
 */
export function referenceProblem(
  schema: FormSchema,
  choice: RefChoice,
  offered: ReferenceBundle[] | null,
  loadError: string | null = null,
): string | null {
  if (!usesBundle(schema, choice)) return null;
  if (loadError) return "couldn't load the references";
  if (!choice.bundle) return 'choose one';
  if (offered && !offered.some((b) => b.id === choice.bundle))
    return `${choice.bundle} is not available for this workflow`;
  return null;
}

/**
 * The choice that loads with `params` (an example's, or imported ones): the named bundle, else own
 * files when they set a param a bundle would fill, else `current`.
 */
export function choiceFor(
  schema: FormSchema,
  bundle: string | null | undefined,
  params: Record<string, unknown>,
  current: RefChoice,
): RefChoice {
  const refs = schema.references;
  if (!refs) return current;
  if (bundle) return { bundle, custom: false };
  const setsFilled = Object.keys(refs.fill).some((p) => params[p] != null);
  return setsFilled && refs.custom ? { bundle: null, custom: true } : current;
}
