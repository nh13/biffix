import { afterEach, describe, expect, it, vi } from 'vitest';
import { NO_CHOICE } from '../referenceChoice';
import { ExampleChoice, type ExampleForm } from './exampleChoice.svelte';
import type { FormSnapshot } from './formEntry';

const entered = (input: string): FormSnapshot => ({
  values: { input },
  tables: {},
  sheetModes: {},
  refChoice: NO_CHOICE,
  retention: 'default',
  experiment: null,
  runName: 'run',
  from: null,
  notice: null,
  touched: { input: true },
});

/** A stand-in form: what is entered, as plain data, and what the examples did to it. */
function fakeForm(start: FormSnapshot | null) {
  const form = {
    now: start ?? entered(''),
    hasInput: start !== null,
    filled: [] as number[],
    restored: [] as FormSnapshot[],
  };
  const view: ExampleForm & { choice?: ExampleChoice } = {
    name: 'wf',
    get hasInput() {
      return form.hasInput;
    },
    snapshot: () => structuredClone(form.now),
    fill: (i) => {
      view.choice!.reset();
      form.filled.push(i);
      form.now = entered(`example ${i}`);
      form.hasInput = true;
    },
    restore: (s) => {
      form.restored.push(s);
      form.now = s;
    },
  };
  return { form, view };
}

function choiceOver(view: ExampleForm & { choice?: ExampleChoice }) {
  const choice = new ExampleChoice(view);
  view.choice = choice;
  return choice;
}

afterEach(() => vi.unstubAllGlobals());

describe('ExampleChoice', () => {
  it('fills an example into a fresh form at once, with nothing to undo', () => {
    const { form, view } = fakeForm(null);
    const choice = choiceOver(view);
    choice.setOpen(true);
    choice.choose(1);
    expect(form.filled).toEqual([1]);
    expect(choice.open).toBe(false);
    expect(choice.before).toBeNull();
  });

  it('asks before replacing what was entered, and undoes it after', () => {
    const { form, view } = fakeForm(entered('mine'));
    const choice = choiceOver(view);
    choice.choose(0);
    expect(choice.pending).toBe(0);
    expect(form.filled).toEqual([]);
    // Choosing another example asks about that one instead.
    choice.choose(1);
    expect(choice.pending).toBe(1);
    // Asked twice: replace.
    choice.choose(1);
    expect(form.filled).toEqual([1]);
    expect(choice.pending).toBeNull();
    expect(choice.before).toEqual(entered('mine'));
    choice.undo();
    expect(form.restored).toEqual([entered('mine')]);
    expect(choice.before).toBeNull();
  });

  it('asks before an undo that loses an edit made since, and keeps the example when declined', () => {
    const ask = vi.fn(() => false);
    vi.stubGlobal('confirm', ask);
    const { form, view } = fakeForm(entered('mine'));
    const choice = choiceOver(view);
    choice.choose(0);
    choice.choose(0);
    form.now = { ...form.now, values: { input: 'edited' } };
    choice.undo();
    expect(ask).toHaveBeenCalledOnce();
    expect(form.restored).toEqual([]);
    expect(choice.before).toEqual(entered('mine'));
    ask.mockReturnValue(true);
    choice.undo();
    expect(form.restored).toEqual([entered('mine')]);
  });

  it('undoes without asking when nothing was edited since', () => {
    const ask = vi.fn(() => false);
    vi.stubGlobal('confirm', ask);
    const { form, view } = fakeForm(entered('mine'));
    const choice = choiceOver(view);
    choice.choose(0);
    choice.choose(0);
    choice.undo();
    expect(ask).not.toHaveBeenCalled();
    expect(form.restored).toEqual([entered('mine')]);
  });

  it('drops the replace it was asking about when the menu opens or closes', () => {
    const { view } = fakeForm(entered('mine'));
    const choice = choiceOver(view);
    choice.choose(0);
    choice.setOpen(false);
    expect(choice.pending).toBeNull();
  });
});
