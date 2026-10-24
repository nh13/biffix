// The Run tab's form state, shared by its two layouts: the Expert view's long
// form and the Lab view's wizard, which are one form shown two ways. It holds
// what is entered (values, samplesheet rows, the reference, retention, the experiment, the run's
// name), how it loads (a Re-run/Resume pre-fill, else a restored draft, else a fresh form), what is
// sent and what blocks it, the examples and their undo, the params YAML round-trip, the cost
// estimate, the wizard's step navigation, and the launch itself. Switching view re-renders the
// layout over this one instance, so nothing entered is lost.
//
// Construct it while a component initialises (`RunForm`): its effects and its `afterNavigate` hook
// live as long as that component. The form's parts read it from context (`launchForm()`).

import { getContext, setContext, tick, untrack } from 'svelte';
import { afterNavigate, goto } from '$app/navigation';
import { page } from '$app/state';
import { getRun, submitRun, type Experiment } from '#lib/api/index.js';
import { withExperiment } from '#lib/experiments/experiments.js';
import { uploads } from '#lib/upload/store.js';
import { pendingProblem } from '#lib/upload/uploads.svelte.js';
import { saveBlob } from '#lib/util/download.js';
import { errorMessage } from '#lib/util/format.js';
import { readPref, writePref } from '#lib/util/storage.js';
import { replaceUrl } from '#lib/util/replaceUrl.js';
import { runTabHref } from '#lib/runs/runNav.js';
import { parseSchema } from '#lib/workflows/schema.js';
import {
  clearDraft,
  draftValues,
  firstProblems,
  formHasInput,
  isChanged,
  labelOf,
  launchParts,
  launchProblems,
  layout,
  loadDraft,
  paramsFrom,
  paramsYaml,
  parseParamsYaml,
  pathSheetModes,
  prefillFromRun,
  prefillValues,
  problemOrder,
  runNameFrom,
  saveDraft,
  secretsToReenter,
  launchRequest,
  uploadingProblems,
  valuesFromExample,
  type FormValues,
  type LaunchTarget,
  type Problem,
} from '../form';
import {
  bundleParams,
  choiceFor,
  chosenBundle,
  NO_CHOICE,
  REFERENCE_FIELD,
  referenceProblem,
  usesBundle,
  type RefChoice,
} from '../referenceChoice';
import { retentionOptions, type Retention } from '../retention';
import { problemColumn, sampleCount, sheetCellId, type SheetRow } from '../samplesheet/sheet';
import {
  canReach,
  firstBlockedStep,
  parseStep,
  placeFields,
  plainClick,
  reviewDetailsSummary,
  reviewOptions,
  reviewReference,
  reviewSampleLines,
  stepCards,
  stepOfField,
  stepProblems,
  type FormStep,
  type WizardStep,
} from '../wizard';
import { CostEstimate } from './costEstimate.svelte';
import { ExperimentChoice } from './experimentChoice.svelte';
import { ReferenceOffers } from './referenceOffers.svelte';

/** What the form is given: read through getters, so they stay the current props. */
export interface LaunchFormProps {
  readonly detail: LaunchTarget;
  /** An unregistered form (`biffix workflows form --open`): fill it in, but never launch or
   * save. */
  readonly preview: boolean;
  /** Render as the Lab view's step-by-step wizard rather than the single long form. */
  readonly wizard: boolean;
}

export type SheetModes = Record<string, 'rows' | 'path'>;

/** Pre-filled from an earlier run: its id, whether this launch resumes it, and what it carried. */
export interface FromRun {
  runId: string;
  resume: boolean;
  secrets: string[];
  images: string[];
}

/** The form as it was before an example replaced it, for *Undo*. */
interface BeforeExample {
  values: FormValues;
  tables: Record<string, SheetRow[]>;
  sheetModes: SheetModes;
  refChoice: RefChoice;
  retention: Retention;
  experiment: string | null;
  runName: string;
  from: FromRun | null;
  notice: string | null;
  touched: Record<string, boolean>;
}

/** The experiment dropdown's *New experiment…* choice. */
export const NEW_EXPERIMENT = '__new__';

// -- Show all parameters (remembered per viewer) ---------------------------------------------
const SHOW_ALL_KEY = 'biffix:launch-show-all';
const readShowAll = (): boolean => readPref(SHOW_ALL_KEY) === '1';
const BANNER_KEY = (name: string) => `biffix:launch-banner-dismissed:${name}`;

export class LaunchForm {
  readonly #p: LaunchFormProps;

  get detail(): LaunchTarget {
    return this.#p.detail;
  }
  get preview(): boolean {
    return this.#p.preview;
  }
  get wizard(): boolean {
    return this.#p.wizard;
  }

  readonly schema = $derived(parseSchema(this.detail.form_schema));
  readonly engine = $derived(this.detail.engine);
  // The version the CLI command names; a preview isn't registered, so it has no id yet.
  readonly cliVersionId = $derived(this.preview ? '<version-id>' : this.detail.id);
  readonly examples = $derived(this.schema.examples ?? []);

  values = $state<FormValues>({});
  /** Samplesheet rows by param, for samplesheet params built as rows. */
  tables = $state<Record<string, SheetRow[]>>({});
  /** Samplesheet params switched to a sheet file's path. */
  sheetModes = $state<SheetModes>({});
  runName = $state('');
  touched = $state<Record<string, boolean>>({});
  attempted = $state(false);
  launching = $state(false);
  launchError = $state<string | null>(null);
  /** Pre-filled from an earlier run: its id, and whether this launch resumes it. */
  from = $state<FromRun | null>(null);
  notice = $state<string | null>(null);
  dirty = $state(false);
  /** The reference dropdown's choice, and the bundles it offers (`null` while loading). */
  refChoice = $state<RefChoice>(NO_CHOICE);
  /** What happens to a Nextflow run's intermediate files once it ends. */
  retention = $state<Retention>('default');
  /** The experiment to launch into (the wizard's Review; `?experiment=` preselects it). */
  experiment = $state<string | null>(null);
  readonly retentionChoices = $derived(
    retentionOptions(this.detail.retention_days, this.retention),
  );
  readonly byName = $derived(new Map(this.schema.fields.map((f) => [f.name, f])));
  readonly refs = $derived(this.schema.references ?? null);
  /** The bundles the reference dropdown offers (`null` while loading). */
  readonly references: ReferenceOffers;
  /** The bundle chosen, once the offered bundles have loaded. */
  readonly chosen = $derived.by(() => chosenBundle(this.refChoice, this.references.offered));
  showAll = $state(readShowAll());
  loadingRun = $state(false);
  /** The launch buttons are off while launching, while a Re-run's pre-fill loads, and in a
   * preview. */
  readonly launchDisabled = $derived(this.launching || this.loadingRun || this.preview);

  // Read as plain strings, so a change of the wizard's `?step=` never reloads the form.
  readonly fromParam = $derived(page.url.searchParams.get('from'));
  readonly resumeParam = $derived(page.url.searchParams.get('resume') === '1');
  // A sample to point out in the samplesheet (a failed analysis's *Fix and try again*).
  readonly highlightSample = $derived(page.url.searchParams.get('sample'));
  // An experiment to launch into (*Start an analysis in this experiment*).
  readonly experimentParam = $derived(page.url.searchParams.get('experiment'));

  // -- The experiment (the wizard's Review) -------------------------------------------------------
  // The version's project's open experiments, once the wizard (or a chosen experiment) needs them.
  readonly project = $derived(this.detail.project ?? null);
  readonly experiments: ExperimentChoice;
  readonly experimentName = $derived.by(
    () => this.experiments.list?.find((e) => e.id === this.experiment)?.name ?? this.experiment,
  );

  // -- Derived: layout, params, problems ---------------------------------------------------------
  // The wizard has no *Show all parameters* (that is the Expert form's): a field the author hid is
  // shown only once it is sent changed or has a problem (`revealed`), so it can be seen and fixed.
  readonly shownAll = $derived(this.showAll && !this.wizard);
  revealed = $state<Set<string>>(new Set());
  readonly lay = $derived(
    layout(this.schema, this.engine, this.values, this.shownAll, {
      omit: bundleParams(this.schema, this.refChoice, this.chosen),
      keep: this.refs?.section ?? null,
      reveal: this.wizard ? this.revealed : undefined,
      // `show_if` judged as the launch judges it, so a field shown is a field checked.
      inactive: this.#inactive(),
    }),
  );
  /** What a fresh form sends (its prefills): what the Review and the changed dots compare with. */
  readonly startParams = $derived(paramsFrom(this.schema, prefillValues(this.schema)));
  // What is sent and what blocks it, from the form's state (`launchParts`, shared with the Lab
  // view's *Try again* so the two launch the same thing): the row-built sheets, the values as the
  // server judges `show_if` on them, the fields those hide, the params (without what biffix fills
  // itself: the bundle's params, the row-built sheets), the validation errors and the rows' own
  // problems.
  readonly parts = $derived(
    launchParts({
      schema: this.schema,
      engine: this.engine,
      values: this.values,
      tables: this.tables,
      sheetModes: this.sheetModes,
      reference: this.refChoice,
      bundle: this.chosen,
    }),
  );
  readonly inactive = $derived(this.parts.inactive);
  readonly rowFields = $derived(this.parts.rowFields);
  readonly params = $derived(this.parts.params);
  readonly errors = $derived(this.parts.errors);
  /** The sheets built as rows that hold any. */
  readonly filledSheets = $derived(
    this.rowFields.filter((f) => (this.tables[f.name]?.length ?? 0) > 0),
  );
  // -- Estimate: the samples in the first sheet built as rows, priced from the workflow's past runs
  // (re-asked as the rows change, debounced).
  readonly samples = $derived.by(() => {
    const f = this.filledSheets[0];
    return f ? sampleCount(f.samplesheet!, this.tables[f.name]) : 0;
  });
  readonly estimate: CostEstimate;

  // Files still uploading: a param's path, or a file cell of rows biffix will write as a sheet.
  readonly stillUploading = $derived(
    uploadingProblems(
      this.params,
      this.rowFields.map((f) => ({ name: f.name, rows: this.tables[f.name] ?? [] })),
      (uri) => {
        const p = uploads.pendingFor(uri);
        return p ? pendingProblem(p) : null;
      },
    ),
  );
  // Only a secret this form can take again: a field of this version the current values show.
  readonly secretsMissing = $derived(
    secretsToReenter(this.from?.secrets ?? [], this.byName, this.inactive, this.params),
  );
  readonly refProblem = $derived.by(() =>
    referenceProblem(this.schema, this.refChoice, this.references.offered, this.references.error),
  );
  readonly problems = $derived(
    launchProblems(
      {
        errors: this.errors.map((e) => ({ field: e.field, message: e.message })),
        sheets: this.parts.sheetProblems,
        uploading: this.stillUploading,
        reference: this.refProblem ? { field: REFERENCE_FIELD, message: this.refProblem } : null,
        secretsMissing: this.secretsMissing,
      },
      // In the form's own field order, so the footer names the first problem on the page first.
      problemOrder(this.schema),
    ),
  );
  readonly problemOf = $derived(firstProblems(this.problems));

  // -- Examples ----------------------------------------------------------------------------------
  examplesOpen = $state(false);
  pendingExample = $state<number | null>(null);
  bannerDismissed = $state(false);
  /** Whether an example would overwrite something: what the user entered, or a Re-run's
   * pre-fill. */
  readonly hasInput = $derived(
    this.from !== null ||
      formHasInput(this.schema, {
        values: this.values,
        tables: this.tables,
        reference: this.refChoice,
        extras: {
          runName: this.runName,
          defaultRunName: this.#defaultRunName(),
          retention: this.retention,
          experiment: this.experiment,
          // A linked experiment the project does not offer was dropped, not changed by the user.
          defaultExperiment: this.#startExperiment(),
        },
      }),
  );
  /** The form as it was before an example replaced it, for *Undo*. */
  beforeExample = $state<BeforeExample | null>(null);
  /** The form just after the example went in: an edit since then is lost by *Undo*, so it asks. */
  #exampleState: string | null = null;

  // -- YAML import -------------------------------------------------------------------------------
  importOpen = $state(false);
  importText = $state('');
  importError = $state<string | null>(null);

  // The Expert form's *Show all* opens Advanced; the wizard's starts collapsed.
  advancedOpen = $state(false);
  readonly outputPlaceholder = $derived(
    `Default: this ${this.wizard ? 'analysis' : 'run'}'s own results folder`,
  );

  // -- The wizard (Lab view) ----------------------------------------------------------------------
  // The same form, a step at a time: Add samples (2), Reference & options (3), Review & launch (4).
  // Step 1, picking the pipeline, is New run. `?step=` names the step, so Back, a reload and a
  // shared link all land on it.
  readonly placed = $derived(placeFields(this.schema, this.engine));
  readonly step = $derived(parseStep(page.url.searchParams.get('step')));
  // Back to step 1 keeps the experiment being launched into.
  readonly newRunHref = $derived(withExperiment('/runs/new', this.experiment));
  readonly formStep = $derived<FormStep>(this.step === 1 ? 2 : this.step);
  /** What blocks leaving the current step (on Review, anything that blocks the launch). */
  readonly blocking = $derived(stepProblems(this.problems, this.placed, this.formStep));
  // A step reached by its URL (a link, a reload) that an earlier step blocks goes back to that
  // step, judged once the form has loaded (a Re-run's pre-fill, the reference choices). Only on
  // arrival: a change made on this step never moves it.
  #arrived = $state(true);
  /** The field to focus once the step it is on has rendered (else the step's heading). */
  #pendingFocus: string | Problem | null = null;
  // A step change (Next, Back, the stepper, the browser's Back) moves focus to the new step.
  #shownStep: WizardStep;
  // Step 2 shows the samplesheet (or, without one, the input files); step 3 everything else.
  readonly steps = $derived(stepCards(this.lay, this.placed, this.refs?.section ?? null));

  // -- Review: what the launch will run with ------------------------------------------------------
  reviewMoreOpen = $state(false);
  readonly outdirValue = $derived.by(() => {
    const v = this.lay.outdir ? this.values[this.lay.outdir.name] : undefined;
    return typeof v === 'string' ? v : null;
  });
  readonly reviewSummary = $derived(
    reviewDetailsSummary({
      outdir: this.outdirValue,
      resume: !!this.from?.resume,
      retention:
        this.engine === 'nextflow'
          ? (this.retentionChoices.find((o) => o.value === this.retention)?.label ?? null)
          : null,
    }),
  );
  readonly reviewSamples = $derived(
    reviewSampleLines(this.steps.samples, {
      sheetModes: this.sheetModes,
      tables: this.tables,
      params: this.params,
    }),
  );
  readonly reviewRef = $derived(
    reviewReference(this.schema, {
      byName: this.byName,
      choice: this.refChoice,
      bundle: this.chosen,
      inactive: this.inactive,
      params: this.params,
    }),
  );
  readonly reviewChanged = $derived(
    reviewOptions(this.schema, {
      at: this.placed,
      choice: this.refChoice,
      bundle: this.chosen,
      inactive: this.inactive,
      params: this.params,
      start: this.startParams,
    }),
  );

  constructor(props: LaunchFormProps) {
    this.#p = props;
    this.advancedOpen = untrack(() => !props.wizard) && readShowAll();
    this.#shownStep = untrack(() => this.step);

    // -- Loading: a Re-run/Resume pre-fill, else a restored draft, else a fresh form --------------
    $effect(() => {
      const versionId = this.detail.id;
      const fromRun = this.fromParam;
      const resume = this.resumeParam;
      // A run that loads after the page moved on (another version, or another run) is ignored.
      let cancelled = false;
      untrack(() => {
        this.resetForm();
        this.loadingRun = false;
        if (this.preview) return;
        if (fromRun) {
          this.loadingRun = true;
          getRun(fromRun, { tasks: false })
            .then((run) => {
              if (cancelled) return;
              const p = prefillFromRun(run, this.schema, resume);
              this.values = p.values;
              this.tables = p.tables;
              this.refChoice = p.reference;
              this.retention = p.retention;
              this.#pathModeFor(Object.keys(p.values));
              this.runName = p.runName;
              // Trying a run again lands in its experiment, unless the link names another.
              this.experiment = this.experimentParam ?? p.experiment;
              this.from = { runId: run.run_id, resume, secrets: p.secrets, images: p.images };
              // Choosing an example now asks before replacing the pre-filled form.
              this.dirty = true;
            })
            .catch((e) => {
              if (!cancelled) this.launchError = `Couldn't load ${fromRun}: ${errorMessage(e)}`;
            })
            .finally(() => {
              if (!cancelled) this.loadingRun = false;
            });
          return;
        }
        const draft = loadDraft(versionId);
        // A draft holding anything the user entered (a value, a row, a reference, a name, a
        // retention or an experiment), whether or not the author declared prefills.
        if (
          draft &&
          formHasInput(this.schema, {
            values: draft.values,
            tables: draft.tables ?? {},
            reference: draft.reference ?? NO_CHOICE,
            extras: {
              runName: draft.runName,
              defaultRunName: this.#defaultRunName(),
              retention: draft.retention ?? 'default',
              experiment: draft.experiment ?? null,
              defaultExperiment: this.experimentParam,
            },
          })
        ) {
          this.values = draft.values;
          this.tables = draft.tables ?? {};
          this.sheetModes = draft.sheetModes ?? {};
          this.refChoice = draft.reference ?? NO_CHOICE;
          this.retention = draft.retention ?? 'default';
          this.runName = draft.runName || this.runName;
          this.experiment = this.experimentParam ?? draft.experiment ?? null;
          this.dirty = true;
          this.notice = 'draft';
        }
      });
      return () => {
        cancelled = true;
      };
    });

    // The bundles this form's reference dropdown can offer: of its kind, with every key it
    // requires. Nothing to pick from: open on the params' own fields, when the author allows it.
    this.references = new ReferenceOffers(
      () => this.refs,
      () => {
        if (!this.refChoice.bundle) this.refChoice = { bundle: null, custom: true };
      },
    );

    // The version's project's open experiments, once the wizard (or a chosen experiment) needs
    // them.
    const form = this;
    this.experiments = new ExperimentChoice({
      get project() {
        return form.project;
      },
      get needed() {
        return !form.preview && !!form.project && (form.wizard || form.experiment !== null);
      },
      get experiment() {
        return form.experiment;
      },
      set experiment(v) {
        form.experiment = v;
      },
    });

    // Keep the draft as the form changes (never for a pre-filled Re-run, which has its own source).
    $effect(() => {
      const snapshot = {
        values: draftValues(this.schema, $state.snapshot(this.values) as FormValues),
        runName: this.runName,
        tables: $state.snapshot(this.tables) as Record<string, SheetRow[]>,
        sheetModes: $state.snapshot(this.sheetModes) as SheetModes,
        reference: $state.snapshot(this.refChoice) as RefChoice,
        retention: this.retention,
        experiment: this.experiment,
      };
      if (!this.dirty || this.from || this.preview) return;
      saveDraft(this.detail.id, snapshot);
    });

    this.estimate = new CostEstimate({
      get id() {
        return form.detail.id;
      },
      get samples() {
        return form.samples;
      },
      get preview() {
        return form.preview;
      },
    });

    // Once revealed, a hidden field stays shown until the form is reset, so it never vanishes while
    // it is being fixed.
    $effect(() => {
      if (!this.wizard) return;
      const add = this.schema.fields
        .filter(
          (f) =>
            f.hidden && (this.problemOf.has(f.name) || isChanged(f, this.params, this.startParams)),
        )
        .map((f) => f.name);
      untrack(() => {
        if (add.some((n) => !this.revealed.has(n)))
          this.revealed = new Set([...this.revealed, ...add]);
      });
    });

    $effect(() => {
      const name = this.detail.name;
      untrack(() => {
        this.bannerDismissed = readPref(BANNER_KEY(name)) === '1';
      });
    });

    // Switching to the wizard collapses what the Expert form left open.
    $effect(() => {
      if (this.wizard) untrack(() => (this.advancedOpen = false));
    });

    $effect(() => {
      if (this.wizard && this.step === 1) goto(this.newRunHref, { replace: true });
    });
    $effect(() => {
      if (
        !this.wizard ||
        !this.#arrived ||
        this.loadingRun ||
        (this.refs && this.references.offered === null)
      )
        return;
      this.#arrived = false;
      if (this.step === 1 || canReach(this.step, this.problems, this.placed)) return;
      const blocked = firstBlockedStep(this.problems, this.placed);
      if (blocked) replaceUrl(this.stepHref(blocked));
    });
    afterNavigate(() => {
      this.#arrived = true;
      const s = this.step;
      if (this.wizard && s !== this.#shownStep) void this.#focusStep();
      this.#shownStep = s;
    });
  }

  #inactive(): Set<string> {
    return this.parts.inactive;
  }

  /** The experiment a fresh form starts in: the linked one, unless the project does not offer it. */
  #startExperiment(): string | null {
    return this.experiments.unavailable === this.experimentParam ? null : this.experimentParam;
  }

  #defaultRunName(): string {
    return runNameFrom(this.schema.run_name, this.detail.name, new Date());
  }

  /** Put every piece of the form back to a fresh form's state. */
  resetForm() {
    this.values = prefillValues(this.schema);
    this.tables = {};
    // A samplesheet the author prefilled is a file: it shows, and is sent, in path mode.
    this.sheetModes = pathSheetModes(this.schema, Object.keys(this.values));
    this.touched = {};
    this.attempted = false;
    this.launchError = null;
    this.notice = null;
    this.from = null;
    this.dirty = false;
    this.refChoice = NO_CHOICE;
    this.retention = 'default';
    this.experiment = this.experimentParam;
    this.runName = this.#defaultRunName();
    this.pendingExample = null;
    this.beforeExample = null;
    this.revealed = new Set();
  }

  toggleShowAll() {
    this.showAll = !this.showAll;
    // Showing everything includes what Advanced holds.
    if (this.showAll) this.advancedOpen = true;
    writePref(SHOW_ALL_KEY, this.showAll ? '1' : '0');
  }

  // -- Editing ------------------------------------------------------------------------------------
  setValue(name: string, v: unknown) {
    this.values = { ...this.values, [name]: v };
    this.dirty = true;
  }
  touch(name: string) {
    this.touched = { ...this.touched, [name]: true };
  }
  setRefChoice(choice: RefChoice) {
    this.refChoice = choice;
    this.touched = { ...this.touched, [REFERENCE_FIELD]: true };
    this.dirty = true;
  }
  setRetention(r: Retention) {
    this.retention = r;
    this.dirty = true;
  }
  /** A samplesheet param switched between rows and an existing sheet file's path. */
  setSheetMode(name: string, m: 'rows' | 'path') {
    this.sheetModes = { ...this.sheetModes, [name]: m };
    // Rows replace a path given before: switching back asks for a path again rather than
    // sending the old one in place of the rows.
    if (m === 'rows' && this.values[name] !== undefined) {
      const next = { ...this.values };
      delete next[name];
      this.values = next;
    }
    this.dirty = true;
  }
  setRows(name: string, rows: SheetRow[]) {
    this.tables = { ...this.tables, [name]: rows };
    this.dirty = true;
  }
  /** Samplesheet params among `names` that now hold a path: show them as a path, not rows. */
  #pathModeFor(names: string[]) {
    const paths = pathSheetModes(this.schema, names);
    if (Object.keys(paths).length) this.sheetModes = { ...this.sheetModes, ...paths };
  }

  /** A field's problem, once it was touched or a launch was attempted. */
  errorFor(name: string): string | null {
    const p = this.problemOf.get(name);
    return p && (this.attempted || this.touched[name]) ? p.message : null;
  }
  /** How a problem list names the field a problem is about. */
  problemLabel(name: string): string {
    if (name === REFERENCE_FIELD) return this.refs?.label ?? 'Reference';
    const f = this.byName.get(name);
    return f ? labelOf(f) : name;
  }

  // -- The experiment -----------------------------------------------------------------------------
  chooseExperiment(value: string) {
    if (value === NEW_EXPERIMENT) {
      this.experiments.creating = true;
      return;
    }
    this.experiment = value || null;
    this.experiments.unavailable = null;
    this.dirty = true;
  }
  experimentCreated(e: Experiment) {
    this.experiments.list = [e, ...(this.experiments.list ?? [])];
    this.experiments.creating = false;
    this.chooseExperiment(e.id);
  }

  // -- Examples -----------------------------------------------------------------------------------
  /** Open or close the examples menu, dropping any replace it was asking about. */
  setExamplesOpen(open: boolean) {
    this.examplesOpen = open;
    this.pendingExample = null;
  }
  dismissBanner() {
    this.bannerDismissed = true;
    writePref(BANNER_KEY(this.detail.name), '1');
  }
  #formState(): string {
    return JSON.stringify(
      $state.snapshot({
        values: this.values,
        tables: this.tables,
        refChoice: this.refChoice,
        retention: this.retention,
        experiment: this.experiment,
        runName: this.runName,
      }),
    );
  }
  undoExample() {
    const b = this.beforeExample;
    if (!b) return;
    if (
      this.#exampleState !== null &&
      this.#formState() !== this.#exampleState &&
      !confirm('Undo the example? The changes you made since will be lost too.')
    )
      return;
    this.#exampleState = null;
    this.values = b.values;
    this.tables = b.tables;
    this.sheetModes = b.sheetModes;
    this.refChoice = b.refChoice;
    this.retention = b.retention;
    this.experiment = b.experiment;
    this.runName = b.runName;
    this.from = b.from;
    this.notice = b.notice === 'draft' ? null : b.notice;
    // The fields the user had touched show their problems again, as before the example.
    this.touched = b.touched;
    this.beforeExample = null;
    this.dirty = true;
  }
  chooseExample(i: number) {
    if (this.hasInput && this.pendingExample !== i) {
      this.pendingExample = i;
      return;
    }
    const ex = this.examples[i];
    // Kept so *Undo* can put back what the example replaced.
    this.beforeExample = this.hasInput
      ? {
          values: $state.snapshot(this.values) as FormValues,
          tables: $state.snapshot(this.tables) as Record<string, SheetRow[]>,
          sheetModes: $state.snapshot(this.sheetModes) as SheetModes,
          refChoice: $state.snapshot(this.refChoice) as RefChoice,
          retention: this.retention,
          experiment: this.experiment,
          runName: this.runName,
          from: $state.snapshot(this.from) as FromRun | null,
          notice: this.notice,
          touched: { ...this.touched },
        }
      : null;
    const keep = this.beforeExample;
    // An example replaces the whole form, including a Re-run's pre-fill and its resume.
    this.resetForm();
    this.beforeExample = keep;
    this.values = valuesFromExample(this.schema, ex);
    this.refChoice = choiceFor(this.schema, ex.reference, this.values, NO_CHOICE);
    this.references.noneFitsAgain();
    // An example names an existing samplesheet file.
    this.sheetModes = pathSheetModes(
      this.schema,
      Object.keys(this.values).filter((n) => this.values[n] !== undefined),
    );
    this.runName = ex.run_name?.trim() || this.#defaultRunName();
    this.dirty = true;
    this.examplesOpen = false;
    this.notice = `example:${ex.name}`;
    this.#exampleState = this.beforeExample ? this.#formState() : null;
  }

  // -- YAML import / download ---------------------------------------------------------------------
  applyImport() {
    try {
      const imported = parseParamsYaml(this.importText, this.schema);
      const kept = Object.fromEntries(
        Object.entries(imported.values).filter(
          ([k]) => this.showAll || !imported.unknown.includes(k),
        ),
      );
      this.values = { ...this.values, ...kept };
      this.refChoice = choiceFor(this.schema, null, kept, this.refChoice);
      this.#pathModeFor(Object.keys(kept));
      this.dirty = true;
      this.importOpen = false;
      this.importText = '';
      this.importError = null;
      this.notice = imported.unknown.length
        ? `imported-unknown:${imported.unknown.join(', ')}`
        : 'imported';
    } catch (e) {
      this.importError = errorMessage(e);
    }
  }
  downloadYaml() {
    const yaml = paramsYaml(this.cliVersionId, this.params, {
      reference: usesBundle(this.schema, this.refChoice) ? this.refChoice.bundle : null,
      retention: this.engine === 'nextflow' ? this.retention : null,
      name: this.runName,
      experiment: this.experiment,
      images: this.from?.images ?? [],
      rowSheets: this.filledSheets.map((f) => f.name),
      secrets: this.schema.fields.filter((f) => f.is_secret).map((f) => f.name),
    });
    saveBlob(new Blob([yaml], { type: 'application/x-yaml' }), 'params.yaml');
  }

  discardDraft() {
    clearDraft(this.detail.id);
    this.resetForm();
    this.references.noneFitsAgain();
    // A fresh form starts at Add samples.
    if (this.wizard && this.step !== 2) this.goStep(2);
  }

  // -- Launch -------------------------------------------------------------------------------------
  /** The cell a samplesheet row's problem is about (its column's, else the row's first). */
  #cellOf(p: Problem): HTMLElement | null {
    if (p.row === undefined) return null;
    const spec = this.byName.get(p.field)?.samplesheet;
    const column = spec ? problemColumn(spec, p.column) : null;
    return column ? document.getElementById(sheetCellId(p.field, p.row, column)) : null;
  }
  /** Go to a problem: a samplesheet row's cell, else its field. */
  focusProblem = (p: Problem) => {
    if (p.row === undefined) {
      this.focusField(p.field);
      return;
    }
    const target = stepOfField(p.field, this.placed);
    if (this.wizard && target !== this.step) {
      this.goStep(target, p);
      return;
    }
    const el = this.#cellOf(p);
    if (!el) {
      this.focusField(p.field);
      return;
    }
    el.scrollIntoView({ block: 'center', behavior: 'smooth' });
    el.focus({ preventScroll: true });
  };
  focusField(name: string) {
    // In the wizard, a field on another step is fixed there.
    if (this.wizard) {
      const target = stepOfField(name, this.placed);
      if (target !== this.step) {
        this.goStep(target, name);
        return;
      }
    } else if (!this.showAll && this.byName.get(name)?.hidden) {
      // A field the author hides is shown so it can be fixed (the wizard reveals it itself).
      this.showAll = true;
    }
    const el = document.getElementById(`field-${name}`);
    if (!el) {
      // A field in a collapsed Advanced group: open it first.
      const details = document.querySelector<HTMLDetailsElement>(`[data-holds~="${name}"]`);
      if (details) details.open = true;
      this.advancedOpen = true;
      requestAnimationFrame(() => document.getElementById(`field-${name}`)?.focus());
      return;
    }
    // A field inside a collapsed disclosure (the Review's *Where results go*) is shown first.
    for (let d = el.closest('details'); d; d = d.parentElement?.closest('details') ?? null)
      if (!d.open) d.open = true;
    el.scrollIntoView({ block: 'center', behavior: 'smooth' });
    el.focus({ preventScroll: true });
  }
  launch = async () => {
    // One launch at a time, and never before a Re-run's pre-fill has loaded.
    if (this.launching || this.loadingRun) return;
    this.attempted = true;
    this.launchError = null;
    if (this.problems.length > 0) {
      this.focusProblem(this.problems[0]);
      return;
    }
    if (this.preview) return;
    const from = this.from;
    const request = launchRequest({
      workflowId: this.detail.id,
      engine: this.engine,
      schema: this.schema,
      parts: this.parts,
      runName: this.runName,
      tables: this.tables,
      reference: this.refChoice,
      from: from && { runId: from.runId, resume: from.resume, images: from.images },
      retention: this.retention,
      experiment: this.experiment,
    });
    this.launching = true;
    try {
      const { run_id } = await submitRun(request);
      clearDraft(this.detail.id);
      await goto(runTabHref(run_id, 'overview'));
    } catch (e) {
      this.launchError = errorMessage(e);
    } finally {
      this.launching = false;
    }
  };
  /** ⌘/Ctrl+Enter launches. */
  keydown = (e: KeyboardEvent) => {
    if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
      // The wizard launches from its Review step only.
      if (this.wizard && this.step !== 4) return;
      // Not behind an open dialog (the file picker), nor from a form nested in this one (the params
      // import box, the wizard's new-experiment form): there the keys are that form's.
      if (document.querySelector('dialog[open]')) return;
      if (e.target instanceof Element && e.target.closest('.import, form')) return;
      e.preventDefault();
      this.launch();
    }
  };

  // -- Wizard navigation --------------------------------------------------------------------------
  stepHref = (n: WizardStep): string => {
    if (n === 1) return this.newRunHref;
    const u = new URL(page.url.href);
    u.searchParams.set('step', String(n));
    return `${u.pathname}${u.search}`;
  };
  /** A step is a link when every step before it is complete. */
  stepLink = (n: WizardStep): string | null =>
    n === 1 || canReach(n, this.problems, this.placed) ? this.stepHref(n) : null;
  stepDone = (n: WizardStep): boolean =>
    n < this.formStep &&
    (n === 1 || stepProblems(this.problems, this.placed, n as FormStep).length === 0);
  goStep = (n: WizardStep, focus?: string | Problem) => {
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
  /** Next: on to the following step, or, while this one has problems, to the first of them. */
  nextStep() {
    if (this.blocking.length > 0) {
      this.attempted = true;
      this.focusProblem(this.blocking[0]);
      return;
    }
    this.goStep((this.formStep + 1) as WizardStep);
  }
  async #focusStep() {
    await tick();
    const target = this.#pendingFocus;
    this.#pendingFocus = null;
    if (typeof target === 'string') this.focusField(target);
    else if (target) this.focusProblem(target);
    else document.getElementById('wizard-heading')?.focus();
  }
}

const KEY = Symbol('launch-form');

/** Make `form` the one the launch form's parts below this component read. */
export function setLaunchForm(form: LaunchForm): void {
  setContext(KEY, form);
}

/** The launch form set by the enclosing `RunForm`. */
export function launchForm(): LaunchForm {
  return getContext<LaunchForm>(KEY);
}
