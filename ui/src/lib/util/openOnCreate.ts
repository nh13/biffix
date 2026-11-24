// A `<details>` open by default without reopening it: the element is opened once, when it is
// created, and a later change to the argument does nothing (the action has no `update`). A reactive
// `open={…}` attribute is set again on every re-render, so a page that refreshes its data would
// reopen what the reader closed.
//
//   <details use:openOnCreate={!item.task_key}>…</details>

import type { Action } from 'svelte/action';

export const openOnCreate: Action<HTMLDetailsElement, boolean> = (node, open) => {
  if (open) node.open = true;
};
